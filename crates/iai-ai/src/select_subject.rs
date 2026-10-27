// AI-based "Select Subject" using local ONNX background-removal models.
//
// A model's session is built once and kept for the next run, whoever asks
// (the Select Subject button, an ID photo's cut-out): building it reads the
// whole model file again. What a run needs beside the model itself (several
// gigabytes for BiRefNet) is given back when the run ends.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Models whose GPU (DirectML) inference has failed this session (e.g. a large
/// model that OOMs on the GPU while it runs fine on the CPU). Keyed by the
/// model file name. Once a model is here, `segment_kept` skips the GPU for it and
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
        crate::ai::ort_ep::build_session(path, true)?
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
    loaded: &dyn Fn(),
) -> Result<(Vec<u8>, bool), String> {
    let (session, on_gpu) = session_for(&spec, path, prefer_gpu)?;
    loaded();
    let result = run_inference(spec, &session, pixels, width, height);
    if result.is_ok() {
        return result.map(|mask| (mask, on_gpu));
    }
    forget_session(spec.file_name);
    if !on_gpu {
        return result.map(|mask| (mask, false));
    }
    block_gpu_for(spec.file_name);
    let (session, _) = session_for(&spec, path, false)?;
    let result = run_inference(spec, &session, pixels, width, height);
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
}

#[derive(Clone, Copy)]
struct ModelSpec {
    short_label: &'static str,
    file_name: &'static str,
    url: &'static str,
    /// SHA-256 of the file at `url`, as its source publishes it. A download
    /// that does not match is refused.
    sha256: &'static str,
    size_hint: &'static str,
    apply_sigmoid: bool,
    soft_mask: bool,
    /// Worth trying on DirectML. BiRefNet is not: it fails there (out of
    /// memory or unsupported) and the CPU retry stalls the app for a beat.
    gpu: bool,
}

impl SelectSubjectModel {
    fn spec(self) -> ModelSpec {
        match self {
            SelectSubjectModel::BiRefNetTiny => ModelSpec {
                short_label: "BiRefNet",
                file_name: "birefnet-general-tiny-epoch_232.onnx",
                url: "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx",
                // The release page gives no digest: hashed from a fresh
                // download of this URL on 2026-10-07 (224 005 088 bytes).
                sha256: "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333",
                size_hint: "~214 MB",
                apply_sigmoid: true,
                soft_mask: true,
                gpu: false,
            },
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum SubjectStatus {
    #[default]
    NoModel,
    Downloading {
        progress: f32,
    },
    Ready,
    LoadingModel,
    Running,
    Error(String),
}

pub struct SelectSubjectEngine {
    pub status: Arc<parking_lot::Mutex<SubjectStatus>>,
    selected_model: SelectSubjectModel,
}

impl Default for SelectSubjectEngine {
    fn default() -> Self {
        Self::new()
    }
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
            status: Arc::new(parking_lot::Mutex::new(status)),
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

    pub fn is_busy(&self) -> bool {
        matches!(
            *self.status.lock(),
            SubjectStatus::Downloading { .. }
                | SubjectStatus::LoadingModel
                | SubjectStatus::Running
        )
    }

    pub fn status_text(&self) -> String {
        let spec = self.selected_model.spec();
        match self.status.lock().clone() {
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

    pub fn download_model_async(&self) {
        {
            let s = self.status.lock();
            match *s {
                SubjectStatus::NoModel | SubjectStatus::Error(_) => {}
                _ => return,
            }
        }

        let status = self.status.clone();
        let model = self.selected_model;
        std::thread::spawn(move || {
            *status.lock() = SubjectStatus::Downloading { progress: 0.0 };
            let progress = |p: f32| *status.lock() = SubjectStatus::Downloading { progress: p };
            *status.lock() = match download_model(model, &progress) {
                Ok(()) => SubjectStatus::Ready,
                Err(e) => SubjectStatus::Error(e),
            };
        });
    }
}

/// Download `model` on the calling thread, check its SHA-256 and put it in
/// place. `progress` gets the share done, 0 to 1, when the size is known.
pub fn download_model(model: SelectSubjectModel, progress: &dyn Fn(f32)) -> Result<(), String> {
    use std::io::{Read, Write};
    let spec = model.spec();
    let model_path = SelectSubjectEngine::model_path_for(model);
    if let Some(parent) = model_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;
    let mut req = client.get(spec.url);
    if let Ok(token) = std::env::var("HF_TOKEN").or_else(|_| std::env::var("HUGGINGFACE_TOKEN"))
        && !token.trim().is_empty()
    {
        req = req.bearer_auth(token.trim());
    }
    let resp = req.send().map_err(|e| format!("download failed: {e}"))?;
    let status_code = resp.status();
    if status_code == reqwest::StatusCode::UNAUTHORIZED
        || status_code == reqwest::StatusCode::FORBIDDEN
    {
        return Err(format!(
            "{} download is not accessible from this network. Place the ONNX file at {}",
            spec.short_label,
            model_path.display()
        ));
    }
    let mut stream = resp
        .error_for_status()
        .map_err(|e| format!("download failed: {e}"))?;

    let total = stream.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let tmp_path = model_path.with_extension("onnx.part");
    let mut file = std::fs::File::create(&tmp_path).map_err(|e| format!("create file: {e}"))?;
    let mut buf = vec![0u8; 65536];
    let fail = |file: std::fs::File, e: String| {
        drop(file);
        let _ = std::fs::remove_file(&tmp_path);
        Err(e)
    };
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let Some(chunk) = buf.get(..n) else {
                    return fail(file, "read past the buffer".to_string());
                };
                if let Err(e) = file.write_all(chunk) {
                    return fail(file, format!("write: {e}"));
                }
                downloaded += n as u64;
                if total > 0 {
                    progress(downloaded as f32 / total as f32);
                }
            }
            Err(e) => return fail(file, format!("read error: {e}")),
        }
    }
    if let Err(e) = file.flush() {
        return fail(file, format!("flush: {e}"));
    }
    drop(file);
    if total > 0 && downloaded != total {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!(
            "incomplete download ({downloaded}/{total} bytes) - please retry"
        ));
    }
    crate::ai::download::install_verified(&tmp_path, &model_path, spec.sha256)
}

impl SelectSubjectModel {
    /// Short name, as the status bar shows it.
    pub fn name(self) -> &'static str {
        self.spec().short_label
    }

    /// SHA-256 of the model file, as checked when it was downloaded.
    pub fn sha256(self) -> &'static str {
        self.spec().sha256
    }

    /// Download size, for messages ("~214 MB").
    pub fn size_hint(self) -> &'static str {
        self.spec().size_hint
    }

    /// The model file is on this machine.
    pub fn is_installed(self) -> bool {
        SelectSubjectEngine::model_path_for(self).is_file()
    }
}

/// Run `model` on `pixels` on the calling thread, as Select Subject does:
/// on the GPU when the user allows it and the model runs there, else (or when
/// the GPU fails) on the CPU, on the session kept between runs. `loaded` is
/// called once the model is in memory. Returns the mask and whether it ran on
/// the GPU.
pub fn segment_kept(
    model: SelectSubjectModel,
    pixels: &[u8],
    width: u32,
    height: u32,
    loaded: &dyn Fn(),
) -> Result<(Vec<u8>, bool), String> {
    let spec = model.spec();
    let path = SelectSubjectEngine::model_path_for(model);
    if !path.is_file() {
        return Err(format!(
            "thiếu model {} ({})",
            spec.short_label, spec.size_hint
        ));
    }
    let prefer_gpu = spec.gpu && crate::ai::ort_ep::prefer_gpu() && !gpu_blocked(spec.file_name);
    run_kept(spec, &path, prefer_gpu, (pixels, width, height), loaded)
}

/// Run `model` on `pixels` on the calling thread and return its mask (soft for
/// BiRefNet). Takes the GPU when `prefer_gpu` and the model runs there,
/// falling back to the CPU the way `segment_kept` does, on the session that
/// one keeps too.
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
    run_kept(spec, &path, prefer_gpu, photo, &|| {}).map(|(mask, _)| mask)
}

fn run_inference(
    spec: ModelSpec,
    session: &Mutex<OrtSession>,
    pixels: &[u8],
    canvas_w: u32,
    canvas_h: u32,
) -> Result<Vec<u8>, String> {
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
        chw[i] = (r - 0.485) / 0.229;
        chw[n + i] = (g - 0.456) / 0.224;
        chw[2 * n + i] = (b - 0.406) / 0.225;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The model, when this machine has it; none on a machine that has yet
    /// to download it.
    fn installed() -> Option<SelectSubjectModel> {
        [SelectSubjectModel::BiRefNetTiny]
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
    #[allow(unsafe_code)] // Win32: process memory counters
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
