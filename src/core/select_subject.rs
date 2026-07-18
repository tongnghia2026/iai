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
}

#[derive(Clone, Copy)]
enum Normalization {
    MinusHalf,
    ImageNet,
}

impl SelectSubjectModel {
    pub const ALL: [SelectSubjectModel; 3] = [
        SelectSubjectModel::Rmbg14,
        SelectSubjectModel::BiRefNetTiny,
        SelectSubjectModel::BiRefNetFull,
    ];

    fn spec(self) -> ModelSpec {
        match self {
            SelectSubjectModel::Rmbg14 => ModelSpec {
                label: "RMBG-1.4 (Light)",
                short_label: "RMBG-1.4",
                file_name: "rmbg_fp16.onnx",
                url: "https://huggingface.co/briaai/RMBG-1.4/resolve/main/onnx/model_fp16.onnx",
                size_hint: "~88 MB",
                normalization: Normalization::MinusHalf,
                apply_sigmoid: false,
                soft_mask: false,
                cache_session: true,
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
        let selected_model = SelectSubjectModel::Rmbg14;
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
