// AI-based "Select Subject" using local ONNX background-removal models.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

type OrtSession = ort::session::Session;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectSubjectModel {
    Rmbg14,
    BiRefNetTiny,
    BiRefNetFull,
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
    cache_session: bool,
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
    pub const ALL: [SelectSubjectModel; 4] = [
        SelectSubjectModel::BiRefNetTiny,
        SelectSubjectModel::BiRefNetFull,
        SelectSubjectModel::Yolo11Seg,
        SelectSubjectModel::Rmbg14,
    ];

    fn spec(self) -> ModelSpec {
        match self {
            // NOTE: RMBG-1.4 is released for NON-COMMERCIAL use only (Bria license).
            // Kept as an option but not the default; do not ship it in a paid build.
            SelectSubjectModel::Rmbg14 => ModelSpec {
                label: "RMBG-1.4 (non-commercial)",
                short_label: "RMBG-1.4",
                file_name: "rmbg_fp16.onnx",
                url: "https://huggingface.co/briaai/RMBG-1.4/resolve/main/onnx/model_fp16.onnx",
                size_hint: "~88 MB",
                normalization: Normalization::MinusHalf,
                apply_sigmoid: false,
                soft_mask: false,
                cache_session: true,
                kind: SubjectKind::BgRemoval,
            },
            SelectSubjectModel::BiRefNetTiny => ModelSpec {
                label: "BiRefNet Tiny (Quality)",
                short_label: "BiRefNet",
                file_name: "birefnet-general-tiny-epoch_232.onnx",
                url: "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx",
                size_hint: "~214 MB",
                normalization: Normalization::ImageNet,
                apply_sigmoid: true,
                soft_mask: true,
                cache_session: false,
                kind: SubjectKind::BgRemoval,
            },
            SelectSubjectModel::BiRefNetFull => ModelSpec {
                label: "BiRefNet Full (Max Quality)",
                short_label: "BiRefNet Full",
                file_name: "birefnet-general-epoch_244.onnx",
                url: "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet-general-epoch_244.onnx",
                size_hint: "~928 MB",
                normalization: Normalization::ImageNet,
                apply_sigmoid: true,
                soft_mask: true,
                cache_session: false,
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
                cache_session: true,
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

type InferencePayload = (Option<Arc<Mutex<OrtSession>>>, Result<Vec<u8>, String>);

pub struct SelectSubjectEngine {
    pub status: Arc<Mutex<SubjectStatus>>,
    result_rx: Option<Receiver<InferencePayload>>,
    session: Option<Arc<Mutex<OrtSession>>>,
    selected_model: SelectSubjectModel,
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
            session: None,
            selected_model,
        }
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
        self.session = None;
        self.result_rx = None;
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

    fn load_session_from_path(path: &Path) -> Result<Arc<Mutex<OrtSession>>, String> {
        let session = OrtSession::builder()
            .map_err(|e| format!("ORT builder: {e}"))?
            .commit_from_file(path)
            .map_err(|e| format!("ORT load model: {e}"))?;
        Ok(Arc::new(Mutex::new(session)))
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

    pub fn run_async(&mut self, pixels: Vec<u8>, canvas_w: u32, canvas_h: u32) -> bool {
        if self.is_busy() {
            return false;
        }

        let spec = self.selected_model.spec();
        let existing_session = if spec.cache_session {
            self.session.clone()
        } else {
            None
        };
        let model_path = self.model_path();
        let is_first_load = existing_session.is_none();

        *self.status.lock().unwrap() = if is_first_load {
            SubjectStatus::LoadingModel
        } else {
            SubjectStatus::Running
        };
        let status = self.status.clone();

        let (tx, rx): (Sender<InferencePayload>, Receiver<InferencePayload>) = mpsc::channel();
        self.result_rx = Some(rx);

        std::thread::spawn(move || {
            let sess_arc = if let Some(s) = existing_session {
                s
            } else {
                match Self::load_session_from_path(&model_path) {
                    Ok(s) => {
                        *status.lock().unwrap() = SubjectStatus::Running;
                        s
                    }
                    Err(e) => {
                        *status.lock().unwrap() = SubjectStatus::Error(e.clone());
                        let _ = tx.send((None, Err(e)));
                        return;
                    }
                }
            };

            let new_session = if is_first_load && spec.cache_session {
                Some(sess_arc.clone())
            } else {
                None
            };

            let result = run_inference(spec, &sess_arc, &pixels, canvas_w, canvas_h);
            match &result {
                Ok(_) => *status.lock().unwrap() = SubjectStatus::Ready,
                Err(e) => *status.lock().unwrap() = SubjectStatus::Error(e.clone()),
            }
            let _ = tx.send((new_session, result));
        });

        true
    }

    pub fn poll_result(&mut self) -> Option<Result<Vec<u8>, String>> {
        let rx = self.result_rx.as_ref()?;
        match rx.try_recv() {
            Ok((new_sess, result)) => {
                self.result_rx = None;
                if let Some(s) = new_sess {
                    self.session = Some(s);
                }
                Some(result)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.result_rx = None;
                Some(Err("inference thread disconnected unexpectedly".into()))
            }
        }
    }
}

fn run_inference(
    spec: ModelSpec,
    session: &Mutex<OrtSession>,
    pixels: &[u8],
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
    if spec.kind == SubjectKind::YoloSeg {
        return run_yolo_seg(session, pixels, canvas_w, canvas_h);
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

        let outputs = sess
            .run(ort::inputs![input_name.as_str() => tensor])
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
        let outputs = sess
            .run(ort::inputs![input_name.as_str() => tensor])
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
        let mut best = 0.0f32;
        for c in 0..NUM_CLASSES {
            let s = at(4 + c, a);
            if s > best {
                best = s;
            }
        }
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
