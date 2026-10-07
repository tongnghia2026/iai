// AI-based "Select Subject" using local ONNX background-removal models.
//
// A model's session is built once and kept for the next run, whoever asks
// (the Select Subject button, an ID photo's cut-out): building it reads the
// whole model file again. What a run needs beside the model itself (several
// gigabytes for BiRefNet) is given back when the run ends.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

/// Models whose GPU (DirectML) inference has failed this session (e.g. a large
/// model that OOMs on the GPU while it runs fine on the CPU). Keyed by the
/// model file name. Once a model is here, `run_async` skips the GPU for it and
/// goes straight to CPU, so the user does not pay the failed-GPU cost twice.
fn gpu_blocklist() -> &'static Mutex<HashSet<&'static str>> {
    static S: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashSet::new()))
}

fn gpu_blocked(file_name: &str) -> bool {
    gpu_blocklist()
        .lock()
        .map(|s| s.contains(file_name))
        .unwrap_or(false)
}

fn block_gpu_for(file_name: &'static str) {
    if let Ok(mut s) = gpu_blocklist().lock() {
        s.insert(file_name);
    }
}

type OrtSession = ort::session::Session;

/// A model's session, kept once it is built.
struct Kept {
    file_name: &'static str,
    session: Arc<Mutex<OrtSession>>,
    on_gpu: bool,
}

fn kept_sessions() -> &'static Mutex<Vec<Kept>> {
    static KEPT: OnceLock<Mutex<Vec<Kept>>> = OnceLock::new();
    KEPT.get_or_init(|| Mutex::new(Vec::new()))
}

/// The session kept for a model file, and whether it runs on the GPU.
fn kept_session(file_name: &str) -> Option<(Arc<Mutex<OrtSession>>, bool)> {
    let kept = kept_sessions().lock().ok()?;
    let found = kept.iter().find(|k| k.file_name == file_name)?;
    Some((Arc::clone(&found.session), found.on_gpu))
}

/// Let go of a model's session: it failed, and the next run builds another.
fn forget_session(file_name: &str) {
    if let Ok(mut kept) = kept_sessions().lock() {
        kept.retain(|k| k.file_name != file_name);
    }
}

/// The session of the model at `path`: the one kept, or one built now (on the
/// GPU when `prefer_gpu` and it can be) and kept from here on.
fn session_for(
    spec: &ModelSpec,
    path: &Path,
    prefer_gpu: bool,
) -> Result<(Arc<Mutex<OrtSession>>, bool), String> {
    if let Some(kept) = kept_session(spec.file_name) {
        return Ok(kept);
    }
    let (session, on_gpu) = if prefer_gpu {
        crate::core::ai::ort_ep::build_session(path, true)?
    } else {
        // No memory pattern: with one, a session that is run again sets a
        // gigabyte more aside than the run needs.
        let session = OrtSession::builder()
            .map_err(|e| format!("ORT CPU builder: {e}"))?
            .with_memory_pattern(false)
            .map_err(|e| format!("ORT memory pattern: {e}"))?
            .commit_from_file(path)
            .map_err(|e| format!("ORT load model CPU: {e}"))?;
        (session, false)
    };
    let session = Arc::new(Mutex::new(session));
    if let Ok(mut kept) = kept_sessions().lock() {
        kept.retain(|k| k.file_name != spec.file_name);
        kept.push(Kept {
            file_name: spec.file_name,
            session: Arc::clone(&session),
            on_gpu,
        });
    }
    Ok((session, on_gpu))
}

/// Run `spec`'s model on its kept session, over `photo` (its RGBA pixels,
/// width and height). `loaded` is told once the session is there. A run that
/// fails on the GPU (a large model out of memory there) is made again on the
/// CPU, and the model stays off the GPU; a session that fails is not kept.
/// Returns the mask and whether the GPU made it.
fn run_kept(
    spec: ModelSpec,
    path: &Path,
    prefer_gpu: bool,
    (pixels, width, height): (&[u8], u32, u32),
    people_only: bool,
    loaded: &dyn Fn(),
) -> Result<(Vec<u8>, bool), String> {
    let (session, on_gpu) = session_for(&spec, path, prefer_gpu)?;
    loaded();
    let result = run_inference(spec, &session, pixels, width, height, people_only);
    if result.is_ok() {
        return result.map(|mask| (mask, on_gpu));
    }
    forget_session(spec.file_name);
    if !on_gpu {
        return result.map(|mask| (mask, false));
    }
    block_gpu_for(spec.file_name);
    let (session, _) = session_for(&spec, path, false)?;
    let result = run_inference(spec, &session, pixels, width, height, people_only);
    if result.is_err() {
        forget_session(spec.file_name);
    }
    result.map(|mask| (mask, false))
}

/// Options for a run that gives the runtime's working memory back when it
/// ends: kept, it would stay with the session for as long as that lives.
fn run_options() -> Result<ort::session::RunOptions, String> {
    let mut options =
        ort::session::RunOptions::new().map_err(|e| format!("ORT run options: {e}"))?;
    options
        .add_config_entry("memory.enable_memory_arena_shrinkage", "cpu:0")
        .map_err(|e| format!("ORT run options: {e}"))?;
    Ok(options)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectSubjectModel {
    BiRefNetTiny,
    Yolo11Seg,
}

#[derive(Clone, Copy)]
struct ModelSpec {
    label: &'static str,
    short_label: &'static str,
    file_name: &'static str,
    url: &'static str,
    size_hint: &'static str,
    normalization: Normalization,
    apply_sigmoid: bool,
    soft_mask: bool,
    /// Worth trying on DirectML. BiRefNet is not: it fails there (out of
    /// memory or unsupported) and the CPU retry stalls the app for a beat.
    gpu: bool,
    kind: SubjectKind,
}

#[derive(Clone, Copy)]
enum Normalization {
    MinusHalf,
    ImageNet,
}

#[derive(Clone, Copy, PartialEq)]
enum SubjectKind {
    /// Single foreground-matte model (one [1,1,H,W]-style mask output).
    BgRemoval,
    /// Ultralytics YOLO segmentation head (two outputs: detections + mask protos).
    YoloSeg,
}

impl SelectSubjectModel {
    pub const ALL: [SelectSubjectModel; 2] = [
        SelectSubjectModel::BiRefNetTiny,
        SelectSubjectModel::Yolo11Seg,
    ];

    fn spec(self) -> ModelSpec {
        match self {
            SelectSubjectModel::BiRefNetTiny => ModelSpec {
                label: "BiRefNet Tiny (Quality)",
                short_label: "BiRefNet",
                file_name: "birefnet-general-tiny-epoch_232.onnx",
                url: "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx",
                size_hint: "~214 MB",
                normalization: Normalization::ImageNet,
                apply_sigmoid: true,
                soft_mask: true,
                gpu: false,
                kind: SubjectKind::BgRemoval,
            },
            // Ultralytics YOLO11-seg (COCO, AGPL-3.0) — detects objects and unions
            // their instance masks into the selection. Downloaded from a mirror of
            // the standard 640 export; `scripts/export_yolo_seg_onnx.py` reproduces it.
            SelectSubjectModel::Yolo11Seg => ModelSpec {
                label: "YOLO11-seg (objects)",
                short_label: "YOLO11-seg",
                file_name: "yolo11-seg.onnx",
                url: "https://huggingface.co/AXERA-TECH/YOLO11-Seg/resolve/main/yolo11s-seg.onnx",
                size_hint: "~40 MB",
                normalization: Normalization::MinusHalf,
                apply_sigmoid: false,
                soft_mask: false,
                gpu: true,
                kind: SubjectKind::YoloSeg,
            },
        }
    }

    pub fn label(self) -> &'static str {
        self.spec().label
    }

    pub fn short_label(self) -> &'static str {
        self.spec().short_label
    }
}

#[derive(Clone, Debug)]
pub enum SubjectStatus {
    NoModel,
    Downloading { progress: f32 },
    Ready,
    LoadingModel,
    Running,
    Error(String),
}

impl Default for SubjectStatus {
    fn default() -> Self {
        SubjectStatus::NoModel
    }
}

type InferencePayload = Result<Vec<u8>, String>;

pub struct SelectSubjectEngine {
    pub status: Arc<Mutex<SubjectStatus>>,
    result_rx: Option<Receiver<InferencePayload>>,
    /// Document the in-flight job was started on. The result must land on this
    /// document even if the user switched tabs while inference ran, so the poll
    /// resolves it by id instead of applying to whatever tab is active now.
    pending_doc_id: Option<u32>,
    /// Whether the current session runs on the GPU (DirectML) rather than CPU.
    /// Set by the worker when a session is built; reported to the user so a
    /// strong GPU that is actually being used is visible.
    used_gpu: Arc<AtomicBool>,
    selected_model: SelectSubjectModel,
    /// YOLO only: restrict the selection to the COCO "person" class.
    people_only: bool,
}

impl SelectSubjectEngine {
    pub fn new() -> Self {
        // Default to BiRefNet Tiny: MIT-licensed, so it is safe in a commercial
        // build (unlike RMBG-1.4, which is non-commercial).
        let selected_model = SelectSubjectModel::BiRefNetTiny;
        let status = if Self::model_path_for(selected_model).exists() {
            SubjectStatus::Ready
        } else {
            SubjectStatus::NoModel
        };
        Self {
            status: Arc::new(Mutex::new(status)),
            result_rx: None,
            pending_doc_id: None,
            used_gpu: Arc::new(AtomicBool::new(false)),
            selected_model,
            people_only: false,
        }
    }

    pub fn people_only(&self) -> bool {
        self.people_only
    }

    pub fn set_people_only(&mut self, value: bool) {
        self.people_only = value;
    }

    fn models_dir() -> PathBuf {
        if let Ok(appdata) = std::env::var("APPDATA") {
            PathBuf::from(appdata).join("IAI").join("models")
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("iai")
                .join("models")
        } else {
            PathBuf::from(".").join("models")
        }
    }

    pub fn model_path_for(model: SelectSubjectModel) -> PathBuf {
        Self::models_dir().join(model.spec().file_name)
    }

    pub fn model_path(&self) -> PathBuf {
        Self::model_path_for(self.selected_model)
    }

    pub fn selected_model(&self) -> SelectSubjectModel {
        self.selected_model
    }

    pub fn set_selected_model(&mut self, model: SelectSubjectModel) -> bool {
        if self.is_busy() {
            return false;
        }
        if self.selected_model == model {
            return true;
        }
        self.selected_model = model;
        self.result_rx = None;
        self.pending_doc_id = None;
        self.refresh_status_from_disk();
        true
    }

    fn refresh_status_from_disk(&self) {
        let status = if self.model_path().exists() {
            SubjectStatus::Ready
        } else {
            SubjectStatus::NoModel
        };
        *self.status.lock().unwrap() = status;
    }

    pub fn is_busy(&self) -> bool {
        matches!(
            *self.status.lock().unwrap(),
            SubjectStatus::Downloading { .. }
                | SubjectStatus::LoadingModel
                | SubjectStatus::Running
        )
    }

    pub fn status_text(&self) -> String {
        let spec = self.selected_model.spec();
        match self.status.lock().unwrap().clone() {
            SubjectStatus::NoModel => format!(
                "Select Subject: {} not downloaded (click to download {})",
                spec.short_label, spec.size_hint
            ),
            SubjectStatus::Downloading { progress } => {
                format!(
                    "Downloading {} ... {:.0}%",
                    spec.short_label,
                    progress * 100.0
                )
            }
            SubjectStatus::Ready => format!("Select Subject: {} ready", spec.short_label),
            SubjectStatus::LoadingModel => {
                format!(
                    "Select Subject: loading {} into memory...",
                    spec.short_label
                )
            }
            SubjectStatus::Running => format!("Select Subject: running {} ...", spec.short_label),
            SubjectStatus::Error(e) => format!("Select Subject error: {}", e),
        }
    }

    /// True when the last-built session runs on the GPU (DirectML).
    pub fn used_gpu(&self) -> bool {
        self.used_gpu.load(Ordering::Relaxed)
    }

    pub fn download_model_async(&self) {
        {
            let s = self.status.lock().unwrap();
            match *s {
                SubjectStatus::NoModel | SubjectStatus::Error(_) => {}
                _ => return,
            }
        }

        let status = self.status.clone();
        let spec = self.selected_model.spec();
        let model_path = self.model_path();

        std::thread::spawn(move || {
            *status.lock().unwrap() = SubjectStatus::Downloading { progress: 0.0 };

            if let Some(parent) = model_path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    *status.lock().unwrap() = SubjectStatus::Error(format!("create dir: {e}"));
                    return;
                }
            }

            let client = match reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    *status.lock().unwrap() = SubjectStatus::Error(format!("HTTP client: {e}"));
                    return;
                }
            };

            let mut req = client.get(spec.url);
            if let Ok(token) =
                std::env::var("HF_TOKEN").or_else(|_| std::env::var("HUGGINGFACE_TOKEN"))
            {
                if !token.trim().is_empty() {
                    req = req.bearer_auth(token.trim());
                }
            }

            let resp = match req.send() {
                Ok(r) => {
                    let status_code = r.status();
                    if status_code == reqwest::StatusCode::UNAUTHORIZED
                        || status_code == reqwest::StatusCode::FORBIDDEN
                    {
                        *status.lock().unwrap() = SubjectStatus::Error(format!(
                            "{} download is not accessible from this network. Place the ONNX file at {}",
                            spec.short_label,
                            model_path.display()
                        ));
                        return;
                    }
                    match r.error_for_status() {
                        Ok(r) => r,
                        Err(e) => {
                            *status.lock().unwrap() =
                                SubjectStatus::Error(format!("download failed: {e}"));
                            return;
                        }
                    }
                }
                Err(e) => {
                    *status.lock().unwrap() = SubjectStatus::Error(format!("download failed: {e}"));
                    return;
                }
            };

            let total = resp.content_length().unwrap_or(0);
            let mut downloaded: u64 = 0;
            let tmp_path = model_path.with_extension("onnx.part");

            use std::io::{Read, Write};
            let mut file = match std::fs::File::create(&tmp_path) {
                Ok(f) => f,
                Err(e) => {
                    *status.lock().unwrap() = SubjectStatus::Error(format!("create file: {e}"));
                    return;
                }
            };

            let mut stream = resp;
            let mut buf = vec![0u8; 65536];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Err(e) = file.write_all(&buf[..n]) {
                            drop(file);
                            let _ = std::fs::remove_file(&tmp_path);
                            *status.lock().unwrap() = SubjectStatus::Error(format!("write: {e}"));
                            return;
                        }
                        downloaded += n as u64;
                        if total > 0 {
                            *status.lock().unwrap() = SubjectStatus::Downloading {
                                progress: downloaded as f32 / total as f32,
                            };
                        }
                    }
                    Err(e) => {
                        drop(file);
                        let _ = std::fs::remove_file(&tmp_path);
                        *status.lock().unwrap() = SubjectStatus::Error(format!("read error: {e}"));
                        return;
                    }
                }
            }

            if let Err(e) = file.flush() {
                drop(file);
                let _ = std::fs::remove_file(&tmp_path);
                *status.lock().unwrap() = SubjectStatus::Error(format!("flush: {e}"));
                return;
            }
            drop(file);

            if total > 0 && downloaded != total {
                let _ = std::fs::remove_file(&tmp_path);
                *status.lock().unwrap() = SubjectStatus::Error(format!(
                    "incomplete download ({downloaded}/{total} bytes) - please retry"
                ));
                return;
            }

            if let Err(e) = std::fs::rename(&tmp_path, &model_path) {
                *status.lock().unwrap() = SubjectStatus::Error(format!("rename: {e}"));
                return;
            }

            *status.lock().unwrap() = SubjectStatus::Ready;
        });
    }

    pub fn run_async(
        &mut self,
        doc_id: u32,
        pixels: Vec<u8>,
        canvas_w: u32,
        canvas_h: u32,
    ) -> bool {
        if self.is_busy() {
            return false;
        }

        let spec = self.selected_model.spec();
        let model_path = self.model_path();
        // The model is read into memory only the first time it is asked for.
        *self.status.lock().unwrap() = if kept_session(spec.file_name).is_none() {
            SubjectStatus::LoadingModel
        } else {
            SubjectStatus::Running
        };
        let status = self.status.clone();
        let people_only = self.people_only;
        // Decide GPU vs CPU on the main thread (reads the cached wgpu adapter
        // decision) and let the worker record which path the session took. Skip
        // the GPU for a model that already failed on it this session.
        let prefer_gpu =
            spec.gpu && crate::core::ai::ort_ep::prefer_gpu() && !gpu_blocked(spec.file_name);
        let used_gpu = self.used_gpu.clone();

        let (tx, rx): (Sender<InferencePayload>, Receiver<InferencePayload>) = mpsc::channel();
        self.result_rx = Some(rx);
        self.pending_doc_id = Some(doc_id);

        std::thread::spawn(move || {
            let loaded = || *status.lock().unwrap() = SubjectStatus::Running;
            let photo = (pixels.as_slice(), canvas_w, canvas_h);
            let result = run_kept(spec, &model_path, prefer_gpu, photo, people_only, &loaded).map(
                |(mask, on_gpu)| {
                    used_gpu.store(on_gpu, Ordering::Relaxed);
                    mask
                },
            );
            match &result {
                Ok(_) => *status.lock().unwrap() = SubjectStatus::Ready,
                Err(e) => *status.lock().unwrap() = SubjectStatus::Error(e.clone()),
            }
            let _ = tx.send(result);
        });

        true
    }

    /// Poll for a finished job. Returns the document id the job was started on
    /// (so the caller applies the mask to that tab, not the one active now)
    /// alongside the inference result.
    pub fn poll_result(&mut self) -> Option<(Option<u32>, Result<Vec<u8>, String>)> {
        let rx = self.result_rx.as_ref()?;
        match rx.try_recv() {
            Ok(result) => {
                self.result_rx = None;
                Some((self.pending_doc_id.take(), result))
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.result_rx = None;
                Some((
                    self.pending_doc_id.take(),
                    Err("inference thread disconnected unexpectedly".into()),
                ))
            }
        }
    }
}

/// Run `model` on `pixels` on the calling thread and return its mask (soft for
/// BiRefNet). Takes the GPU when `prefer_gpu` and the model runs there,
/// falling back to the CPU the way `run_async` does, on the session that one
/// keeps too.
pub fn segment_blocking(
    model: SelectSubjectModel,
    pixels: &[u8],
    width: u32,
    height: u32,
    prefer_gpu: bool,
) -> Result<Vec<u8>, String> {
    let spec = model.spec();
    let path = SelectSubjectEngine::model_path_for(model);
    if !path.is_file() {
        return Err(format!(
            "thiếu model {} ({})",
            spec.short_label, spec.size_hint
        ));
    }
    let prefer_gpu = spec.gpu && prefer_gpu && !gpu_blocked(spec.file_name);
    let photo = (pixels, width, height);
    run_kept(spec, &path, prefer_gpu, photo, false, &|| {}).map(|(mask, _)| mask)
}

fn run_inference(
    spec: ModelSpec,
    session: &Mutex<OrtSession>,
    pixels: &[u8],
    canvas_w: u32,
    canvas_h: u32,
    people_only: bool,
) -> Result<Vec<u8>, String> {
    if spec.kind == SubjectKind::YoloSeg {
        return run_yolo_seg(session, pixels, canvas_w, canvas_h, people_only);
    }
    const MODEL_W: u32 = 1024;
    const MODEL_H: u32 = 1024;
    let n = (MODEL_W * MODEL_H) as usize;

    let img = image::RgbaImage::from_raw(canvas_w, canvas_h, pixels.to_vec())
        .ok_or_else(|| "Cannot build RgbaImage from canvas pixels".to_string())?;
    let resized = image::imageops::resize(
        &img,
        MODEL_W,
        MODEL_H,
        image::imageops::FilterType::Lanczos3,
    );

    let mut chw = vec![0.0f32; 3 * n];
    for (i, px) in resized.pixels().enumerate() {
        let r = px.0[0] as f32 / 255.0;
        let g = px.0[1] as f32 / 255.0;
        let b = px.0[2] as f32 / 255.0;
        match spec.normalization {
            Normalization::MinusHalf => {
                chw[i] = r - 0.5;
                chw[n + i] = g - 0.5;
                chw[2 * n + i] = b - 0.5;
            }
            Normalization::ImageNet => {
                chw[i] = (r - 0.485) / 0.229;
                chw[n + i] = (g - 0.456) / 0.224;
                chw[2 * n + i] = (b - 0.406) / 0.225;
            }
        }
    }

    let tensor =
        ort::value::Tensor::<f32>::from_array(([1i64, 3i64, MODEL_H as i64, MODEL_W as i64], chw))
            .map_err(|e| format!("create input tensor: {e}"))?;

    let flat: Vec<f32> = {
        let mut sess = session
            .lock()
            .map_err(|e| format!("session mutex poisoned: {e}"))?;

        let input_name: String = sess
            .inputs()
            .first()
            .map(|outlet| outlet.name().to_string())
            .unwrap_or_else(|| "input".to_string());

        let options = run_options()?;
        let outputs = sess
            .run_with_options(ort::inputs![input_name.as_str() => tensor], &options)
            .map_err(|e| format!("ORT inference: {e}"))?;

        let (_, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("extract output tensor: {e}"))?;

        data.to_vec()
    };

    let mut mask_img = image::GrayImage::new(MODEL_W, MODEL_H);
    for (i, &raw) in flat.iter().take(n).enumerate() {
        let v = if spec.apply_sigmoid {
            1.0 / (1.0 + (-raw).exp())
        } else {
            raw
        };
        let x = (i % MODEL_W as usize) as u32;
        let y = (i / MODEL_W as usize) as u32;
        mask_img.put_pixel(x, y, image::Luma([(v * 255.0).clamp(0.0, 255.0) as u8]));
    }

    let canvas_mask = image::imageops::resize(
        &mask_img,
        canvas_w,
        canvas_h,
        image::imageops::FilterType::Lanczos3,
    );

    let result: Vec<u8> = if spec.soft_mask {
        canvas_mask.pixels().map(|p| p.0[0]).collect()
    } else {
        canvas_mask
            .pixels()
            .map(|p| if p.0[0] >= 127 { 255u8 } else { 0u8 })
            .collect()
    };

    Ok(result)
}

/// Run an Ultralytics YOLO segmentation model (standard 640 export) and return a
/// canvas-sized selection mask that is the union of every detected object's
/// instance mask. Layout follows the documented head: input `[1,3,640,640]`
/// (RGB /255, letterboxed), detection output `[1, 4+80+32, 8400]` and mask
/// prototypes `[1, 32, 160, 160]`. Outputs are matched by length so the two can
/// arrive in either order.
fn run_yolo_seg(
    session: &Mutex<OrtSession>,
    pixels: &[u8],
    canvas_w: u32,
    canvas_h: u32,
    people_only: bool,
) -> Result<Vec<u8>, String> {
    const S: u32 = 640; // model input side
    const PROTO: usize = 160; // mask prototype side (S / 4)
    const MASK_DIM: usize = 32; // mask coefficients
    const NUM_CLASSES: usize = 80; // COCO
    const FEAT: usize = 4 + NUM_CLASSES + MASK_DIM; // 116
    const ANCHORS: usize = 8400; // 80^2 + 40^2 + 20^2 at 640
    const CONF: f32 = 0.25;
    const IOU: f32 = 0.5;

    if canvas_w == 0 || canvas_h == 0 {
        return Err("empty canvas".into());
    }
    let img = image::RgbaImage::from_raw(canvas_w, canvas_h, pixels.to_vec())
        .ok_or_else(|| "Cannot build RgbaImage from canvas pixels".to_string())?;

    // Letterbox to S×S keeping aspect, padding with gray 114.
    let scale = (S as f32 / canvas_w as f32).min(S as f32 / canvas_h as f32);
    let new_w = ((canvas_w as f32) * scale).round().clamp(1.0, S as f32) as u32;
    let new_h = ((canvas_h as f32) * scale).round().clamp(1.0, S as f32) as u32;
    let pad_x = (S - new_w) / 2;
    let pad_y = (S - new_h) / 2;
    let resized =
        image::imageops::resize(&img, new_w, new_h, image::imageops::FilterType::Triangle);

    let np = (S * S) as usize;
    let mut chw = vec![114.0f32 / 255.0; 3 * np];
    for y in 0..new_h {
        for x in 0..new_w {
            let px = resized.get_pixel(x, y);
            let idx = (pad_y + y) as usize * S as usize + (pad_x + x) as usize;
            chw[idx] = px.0[0] as f32 / 255.0;
            chw[np + idx] = px.0[1] as f32 / 255.0;
            chw[2 * np + idx] = px.0[2] as f32 / 255.0;
        }
    }

    let tensor = ort::value::Tensor::<f32>::from_array(([1i64, 3i64, S as i64, S as i64], chw))
        .map_err(|e| format!("create input tensor: {e}"))?;

    let (det, protos): (Vec<f32>, Vec<f32>) = {
        let mut sess = session
            .lock()
            .map_err(|e| format!("session mutex poisoned: {e}"))?;
        let input_name: String = sess
            .inputs()
            .first()
            .map(|o| o.name().to_string())
            .unwrap_or_else(|| "images".to_string());
        let options = run_options()?;
        let outputs = sess
            .run_with_options(ort::inputs![input_name.as_str() => tensor], &options)
            .map_err(|e| format!("ORT inference: {e}"))?;
        let (_, a) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("extract out0: {e}"))?;
        let (_, b) = outputs[1]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("extract out1: {e}"))?;
        let (a, b) = (a.to_vec(), b.to_vec());
        let det_len = FEAT * ANCHORS;
        let proto_len = MASK_DIM * PROTO * PROTO;
        if a.len() == det_len && b.len() == proto_len {
            (a, b)
        } else if b.len() == det_len && a.len() == proto_len {
            (b, a)
        } else {
            return Err(format!(
                "unexpected YOLO output sizes ({} / {}); need a standard 640 yolo*-seg export",
                a.len(),
                b.len()
            ));
        }
    };

    struct Det {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        score: f32,
        coeff: [f32; MASK_DIM],
    }
    // Detections are features-major: element (feature f, anchor a) = det[f*ANCHORS + a].
    let at = |f: usize, a: usize| det[f * ANCHORS + a];
    let mut dets: Vec<Det> = Vec::new();
    for a in 0..ANCHORS {
        // COCO class 0 is "person"; features 0..4 are the box, 4.. are class scores.
        let best = if people_only {
            at(4, a)
        } else {
            let mut m = 0.0f32;
            for c in 0..NUM_CLASSES {
                let s = at(4 + c, a);
                if s > m {
                    m = s;
                }
            }
            m
        };
        if best < CONF {
            continue;
        }
        let (cx, cy, w, h) = (at(0, a), at(1, a), at(2, a), at(3, a));
        let mut coeff = [0.0f32; MASK_DIM];
        for (k, slot) in coeff.iter_mut().enumerate() {
            *slot = at(4 + NUM_CLASSES + k, a);
        }
        dets.push(Det {
            x0: cx - w / 2.0,
            y0: cy - h / 2.0,
            x1: cx + w / 2.0,
            y1: cy + h / 2.0,
            score: best,
            coeff,
        });
    }
    if dets.is_empty() {
        return Ok(vec![0u8; (canvas_w * canvas_h) as usize]);
    }

    // Greedy non-maximum suppression by score.
    dets.sort_by(|p, q| {
        q.score
            .partial_cmp(&p.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let iou = |a: &Det, b: &Det| -> f32 {
        let iw = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
        let ih = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
        let inter = iw * ih;
        let area_a = (a.x1 - a.x0).max(0.0) * (a.y1 - a.y0).max(0.0);
        let area_b = (b.x1 - b.x0).max(0.0) * (b.y1 - b.y0).max(0.0);
        let uni = area_a + area_b - inter;
        if uni <= 0.0 {
            0.0
        } else {
            inter / uni
        }
    };
    let mut keep: Vec<usize> = Vec::new();
    let mut removed = vec![false; dets.len()];
    for i in 0..dets.len() {
        if removed[i] {
            continue;
        }
        keep.push(i);
        for j in (i + 1)..dets.len() {
            if !removed[j] && iou(&dets[i], &dets[j]) > IOU {
                removed[j] = true;
            }
        }
    }

    // Union each kept instance's mask (proto @ coeff, sigmoid, box-cropped) at the
    // 160×160 prototype resolution.
    let mut union = vec![0.0f32; PROTO * PROTO];
    let s_proto = PROTO as f32 / S as f32; // 0.25
    for &i in &keep {
        let d = &dets[i];
        let bx0 = (d.x0 * s_proto).floor().clamp(0.0, PROTO as f32 - 1.0) as usize;
        let by0 = (d.y0 * s_proto).floor().clamp(0.0, PROTO as f32 - 1.0) as usize;
        let bx1 = (d.x1 * s_proto).ceil().clamp(0.0, PROTO as f32) as usize;
        let by1 = (d.y1 * s_proto).ceil().clamp(0.0, PROTO as f32) as usize;
        for py in by0..by1 {
            for px in bx0..bx1 {
                let p = py * PROTO + px;
                let mut acc = 0.0f32;
                for (k, &c) in d.coeff.iter().enumerate() {
                    acc += c * protos[k * PROTO * PROTO + p];
                }
                let m = 1.0 / (1.0 + (-acc).exp());
                if m >= 0.5 && m > union[p] {
                    union[p] = m;
                }
            }
        }
    }

    // Prototype mask → letterbox 640 → strip padding → canvas resolution.
    let mut union_img = image::GrayImage::new(PROTO as u32, PROTO as u32);
    for (p, &v) in union.iter().enumerate() {
        union_img.put_pixel(
            (p % PROTO) as u32,
            (p / PROTO) as u32,
            image::Luma([(v * 255.0).clamp(0.0, 255.0) as u8]),
        );
    }
    let up = image::imageops::resize(&union_img, S, S, image::imageops::FilterType::Triangle);
    let cropped = image::imageops::crop_imm(&up, pad_x, pad_y, new_w, new_h).to_image();
    let canvas_mask = image::imageops::resize(
        &cropped,
        canvas_w,
        canvas_h,
        image::imageops::FilterType::Triangle,
    );
    Ok(canvas_mask
        .pixels()
        .map(|p| if p.0[0] >= 128 { 255u8 } else { 0u8 })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model this machine has, the smaller first; none on a machine that
    /// has yet to download one.
    fn installed() -> Option<SelectSubjectModel> {
        [
            SelectSubjectModel::Yolo11Seg,
            SelectSubjectModel::BiRefNetTiny,
        ]
        .into_iter()
        .find(|model| SelectSubjectEngine::model_path_for(*model).is_file())
    }

    #[test]
    fn a_models_session_is_built_once_and_kept_until_it_fails() {
        let Some(model) = installed() else {
            return;
        };
        let (spec, path) = (model.spec(), SelectSubjectEngine::model_path_for(model));
        forget_session(spec.file_name);
        assert!(kept_session(spec.file_name).is_none());
        let (first, on_gpu) = session_for(&spec, &path, false).unwrap();
        assert!(!on_gpu);
        // Asked again, by whoever, it is the same session: nothing is read.
        let (again, _) = session_for(&spec, &path, false).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert!(kept_session(spec.file_name).is_some_and(|(kept, _)| Arc::ptr_eq(&kept, &first)));
        // A run on it works, and leaves it kept.
        let (w, h) = (64u32, 48u32);
        let photo: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % 251) as u8, 90, 60, 255])
            .collect();
        let mask = segment_blocking(model, &photo, w, h, false).unwrap();
        assert_eq!(mask.len(), (w * h) as usize);
        assert!(kept_session(spec.file_name).is_some_and(|(kept, _)| Arc::ptr_eq(&kept, &first)));
        // One that failed is let go of, and the next run builds another.
        forget_session(spec.file_name);
        let (rebuilt, _) = session_for(&spec, &path, false).unwrap();
        assert!(!Arc::ptr_eq(&first, &rebuilt));
        forget_session(spec.file_name);
    }

    #[cfg(windows)]
    fn held_gb() -> (f64, f64) {
        use windows_sys::Win32::System::ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        const GB: f64 = 1024.0 * 1024.0 * 1024.0;
        let mut mine: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
        unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut mine as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            );
        }
        (
            mine.PrivateUsage as f64 / GB,
            mine.PeakPagefileUsage as f64 / GB,
        )
    }

    /// Opt-in: IAI_SUBJECT_PROBE is a photo. Cuts its subject out with
    /// BiRefNet three times, as the app does, and says how long each took and
    /// what memory the process holds after it. IAI_SUBJECT_PAUSE is seconds
    /// to wait between the runs (run back to back, the later ones are slowed
    /// by the processor's own limits, not by the session).
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn probe_session_reuse() {
        let Ok(photo) = std::env::var("IAI_SUBJECT_PROBE") else {
            return;
        };
        let image = image::open(&photo).unwrap().to_rgba8();
        let (w, h) = image.dimensions();
        let pixels = image.into_raw();
        let model = SelectSubjectModel::BiRefNetTiny;
        let pause = std::env::var("IAI_SUBJECT_PAUSE")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        for run in 1..=3 {
            if run > 1 {
                std::thread::sleep(std::time::Duration::from_secs(pause));
            }
            let started = std::time::Instant::now();
            let mask = segment_blocking(model, &pixels, w, h, false).unwrap();
            let on = mask.iter().filter(|m| **m >= 128).count();
            let (held, peak) = held_gb();
            println!(
                "run {run}: {} ms, {on} pixels on; holds {held:.2} GB (peak {peak:.2} GB)",
                started.elapsed().as_millis()
            );
        }
    }
}
