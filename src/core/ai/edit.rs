// Gemini/OpenAI image-edit engine. Each document may own one worker while up to
// `MAX_API_JOBS` workers run concurrently. Finished workers are drained once per
// frame by the app and routed back by stable document id.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use base64::Engine;

use super::settings::{AiProvider, AiSettings};

const MODEL: &str = "gemini-2.5-flash-image";
const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const OPENAI_MODEL: &str = "gpt-image-1";
const OPENAI_EDITS_URL: &str = "https://api.openai.com/v1/images/edits";
/// Longest edge we upload. Gemini downscales internally anyway; this keeps the
/// request small without hurting the edit. The result is resized back to full
/// canvas resolution regardless.
const MAX_UPLOAD_EDGE: u32 = 2048;
const REQUEST_TIMEOUT_SECS: u64 = 180;
pub const MAX_API_JOBS: usize = 3;

/// A finished edit, already resized to the exact target canvas size so masks
/// line up pixel-for-pixel with the Background.
pub struct AiEditResult {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct AiJob {
    pub doc_id: u32,
    #[allow(dead_code)]
    pub width: u32,
    #[allow(dead_code)]
    pub height: u32,
    pub provider: AiProvider,
    pub started: Instant,
    pub output_new_file: bool,
    pub abandoned: bool,
    rx: Receiver<Result<AiEditResult, String>>,
}

pub struct AiFinished {
    pub doc_id: u32,
    pub provider: AiProvider,
    pub output_new_file: bool,
    pub abandoned: bool,
    pub result: Result<AiEditResult, String>,
}

pub struct AiEditEngine {
    jobs: Vec<AiJob>,
    pub settings: AiSettings,
}

impl Default for AiEditEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AiEditEngine {
    pub fn new() -> Self {
        Self {
            jobs: Vec::new(),
            settings: AiSettings::load(),
        }
    }

    /// Includes abandoned workers, which cannot be killed and still need reaping.
    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    pub fn job_count(&self) -> usize {
        self.jobs.len()
    }

    pub fn doc_running(&self, doc_id: u32) -> bool {
        self.job_for_doc(doc_id).is_some()
    }

    pub fn job_for_doc(&self, doc_id: u32) -> Option<&AiJob> {
        self.jobs
            .iter()
            .find(|job| job.doc_id == doc_id && !job.abandoned)
    }

    /// Send `rgba` (`w×h` straight-alpha RGBA8) + `prompt` on a worker thread.
    pub fn run_async(
        &mut self,
        doc_id: u32,
        rgba: Vec<u8>,
        w: u32,
        h: u32,
        prompt: String,
        output_new_file: bool,
    ) -> bool {
        if self.doc_running(doc_id) || self.jobs.len() >= MAX_API_JOBS {
            return false;
        }
        let key = self.settings.active_key().trim().to_string();
        if key.is_empty() || w == 0 || h == 0 {
            return false;
        }
        let provider = self.settings.provider;

        self.spawn_job(
            doc_id,
            w,
            h,
            provider,
            output_new_file,
            move || match provider {
                AiProvider::Gemini => run_blocking(&key, &prompt, rgba, w, h),
                AiProvider::OpenAi => run_blocking_openai(&key, &prompt, rgba, w, h),
            },
        )
    }

    fn spawn_job<F>(
        &mut self,
        doc_id: u32,
        width: u32,
        height: u32,
        provider: AiProvider,
        output_new_file: bool,
        work: F,
    ) -> bool
    where
        F: FnOnce() -> Result<AiEditResult, String> + Send + 'static,
    {
        if self.doc_running(doc_id) || self.jobs.len() >= MAX_API_JOBS {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        self.jobs.push(AiJob {
            doc_id,
            width,
            height,
            provider,
            started: Instant::now(),
            output_new_file,
            abandoned: false,
            rx,
        });
        true
    }

    /// Non-blocking: reap every worker that completed since the previous frame.
    pub fn poll_finished(&mut self) -> Vec<AiFinished> {
        let mut finished = Vec::new();
        let mut i = 0;
        while i < self.jobs.len() {
            let result = match self.jobs[i].rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("worker thread stopped".to_string())),
            };
            if let Some(result) = result {
                let job = self.jobs.remove(i);
                finished.push(AiFinished {
                    doc_id: job.doc_id,
                    provider: job.provider,
                    output_new_file: job.output_new_file,
                    abandoned: job.abandoned,
                    result,
                });
            } else {
                i += 1;
            }
        }
        finished
    }

    /// HTTP workers cannot be killed. Hide this job immediately and discard its
    /// eventual result when the worker channel completes.
    pub fn abandon_doc_job(&mut self, doc_id: u32) -> bool {
        let Some(job) = self
            .jobs
            .iter_mut()
            .find(|job| job.doc_id == doc_id && !job.abandoned)
        else {
            return false;
        };
        job.abandoned = true;
        true
    }
}

fn run_blocking(
    key: &str,
    prompt: &str,
    rgba: Vec<u8>,
    w: u32,
    h: u32,
) -> Result<AiEditResult, String> {
    let img = image::RgbaImage::from_raw(w, h, rgba)
        .ok_or_else(|| "invalid source image buffer".to_string())?;
    let dynimg = downscale(image::DynamicImage::ImageRgba8(img), MAX_UPLOAD_EDGE);

    let mut png = Vec::new();
    dynimg
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("encode PNG: {e}"))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let prompt = super::guarded_edit_prompt(prompt);

    let body = serde_json::json!({
        "contents": [{
            "parts": [
                { "text": prompt },
                { "inlineData": { "mimeType": "image/png", "data": b64 } }
            ]
        }],
        "generationConfig": { "responseModalities": ["TEXT", "IMAGE"] }
    });

    let url = format!("{API_BASE}/{MODEL}:generateContent");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;

    let resp = client
        .post(&url)
        .header("x-goog-api-key", key)
        .json(&body)
        .send()
        .map_err(|e| format!("request failed: {e}"))?;

    let http_status = resp.status();
    let text = resp.text().map_err(|e| format!("read response: {e}"))?;
    if !http_status.is_success() {
        return Err(api_error_message(http_status.as_u16(), &text));
    }

    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse response: {e}"))?;

    // Gemini returns image bytes as a base64 inline-data part (camelCase in the
    // response). Scan all parts since a text part may precede the image.
    let parts = v["candidates"][0]["content"]["parts"]
        .as_array()
        .ok_or_else(|| "no content parts in response".to_string())?;
    let data_b64 = parts
        .iter()
        .find_map(|p| {
            p["inlineData"]["data"]
                .as_str()
                .or_else(|| p["inline_data"]["data"].as_str())
        })
        .ok_or_else(|| "Gemini returned no image (prompt may have been refused)".to_string())?;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.as_bytes())
        .map_err(|e| format!("decode image: {e}"))?;
    let out = image::load_from_memory(&bytes).map_err(|e| format!("load result: {e}"))?;
    let out = out
        .resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        .to_rgba8();

    Ok(AiEditResult {
        rgba: out.into_raw(),
        width: w,
        height: h,
    })
}

fn downscale(img: image::DynamicImage, max_edge: u32) -> image::DynamicImage {
    let (w, h) = (img.width(), img.height());
    let longest = w.max(h);
    if longest <= max_edge {
        return img;
    }
    let scale = max_edge as f32 / longest as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    img.resize(nw, nh, image::imageops::FilterType::Lanczos3)
}

/// OpenAI `gpt-image-1` image edit via the multipart `/v1/images/edits` endpoint.
/// Mirrors the Gemini path: send the flattened canvas PNG + guarded prompt, decode
/// the returned base64 PNG, and resize it back to the exact canvas size so it lines
/// up as a layer over the Background. `size=auto` lets the model match the input's
/// aspect ratio.
fn run_blocking_openai(
    key: &str,
    prompt: &str,
    rgba: Vec<u8>,
    w: u32,
    h: u32,
) -> Result<AiEditResult, String> {
    let img = image::RgbaImage::from_raw(w, h, rgba)
        .ok_or_else(|| "invalid source image buffer".to_string())?;
    let dynimg = downscale(image::DynamicImage::ImageRgba8(img), MAX_UPLOAD_EDGE);

    let mut png = Vec::new();
    dynimg
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("encode PNG: {e}"))?;

    let prompt = super::guarded_edit_prompt(prompt);

    let image_part = reqwest::blocking::multipart::Part::bytes(png)
        .file_name("image.png")
        .mime_str("image/png")
        .map_err(|e| format!("multipart: {e}"))?;
    let form = reqwest::blocking::multipart::Form::new()
        .text("model", OPENAI_MODEL)
        .text("prompt", prompt)
        .text("size", "auto")
        .text("n", "1")
        .part("image", image_part);

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;

    let resp = client
        .post(OPENAI_EDITS_URL)
        .header("Authorization", format!("Bearer {key}"))
        .multipart(form)
        .send()
        .map_err(|e| format!("request failed: {e}"))?;

    let http_status = resp.status();
    let text = resp.text().map_err(|e| format!("read response: {e}"))?;
    if !http_status.is_success() {
        return Err(openai_error_message(http_status.as_u16(), &text));
    }

    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("parse response: {e}"))?;
    let data_b64 = v["data"][0]["b64_json"]
        .as_str()
        .ok_or_else(|| "OpenAI trả về không có ảnh (prompt có thể bị từ chối)".to_string())?;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.as_bytes())
        .map_err(|e| format!("decode image: {e}"))?;
    let out = image::load_from_memory(&bytes).map_err(|e| format!("load result: {e}"))?;
    let out = out
        .resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        .to_rgba8();

    Ok(AiEditResult {
        rgba: out.into_raw(),
        width: w,
        height: h,
    })
}

/// Turn an OpenAI error body into a short, actionable Vietnamese message.
fn openai_error_message(code: u16, body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let msg = v["error"]["message"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    match code {
        401 => "API key OpenAI không hợp lệ (401). Kiểm tra lại key đã dán.".to_string(),
        429 => format!(
            "Hết quota / quá tải OpenAI (429). Kiểm tra billing & rate limit. {}",
            brief(&msg)
        ),
        403 => format!(
            "Bị từ chối (403) — tài khoản OpenAI cần xác minh danh tính (verify organization) \
             mới dùng được gpt-image-1. {}",
            brief(&msg)
        ),
        400 => format!("Yêu cầu không hợp lệ (400) — {}", brief(&msg)),
        _ => format!(
            "OpenAI {code} — {}",
            brief(if msg.is_empty() { body } else { &msg })
        ),
    }
}

/// Turn a Gemini error body into a short, actionable Vietnamese message.
/// Pulls `error.message` and any `RetryInfo.retryDelay`, and adds a tailored
/// hint for the common status codes (esp. 429 quota / 400 bad key / 403).
fn api_error_message(code: u16, body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let msg = v["error"]["message"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();

    let mut retry = String::new();
    if let Some(details) = v["error"]["details"].as_array() {
        for d in details {
            if let Some(rd) = d["retryDelay"].as_str() {
                retry = format!(" Thử lại sau {rd}.");
            }
        }
    }

    match code {
        429 => format!(
            "Hết quota Gemini (429). Model ảnh nano-banana cần bật thanh toán \
             (billing) trong Google AI Studio — free tier giới hạn rất thấp.{retry}"
        ),
        400 if msg.to_lowercase().contains("api key") || msg.contains("API_KEY") => {
            "API key không hợp lệ (400). Kiểm tra lại key đã dán.".to_string()
        }
        403 => format!(
            "Bị từ chối (403) — key chưa có quyền cho model này. {}",
            brief(&msg)
        ),
        _ => format!(
            "API {code} — {}",
            brief(if msg.is_empty() { body } else { &msg })
        ),
    }
}

/// Trim a (possibly huge) API error body to a one-line snippet.
fn brief(text: &str) -> String {
    let t = text.trim().replace('\n', " ");
    if t.len() > 300 {
        format!("{}…", &t[..300])
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_result(width: u32, height: u32) -> Result<AiEditResult, String> {
        Ok(AiEditResult {
            rgba: vec![0; width as usize * height as usize * 4],
            width,
            height,
        })
    }

    fn drain_with_deadline(engine: &mut AiEditEngine, count: usize) -> Vec<AiFinished> {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut out = Vec::new();
        while out.len() < count && Instant::now() < deadline {
            out.extend(engine.poll_finished());
            std::thread::yield_now();
        }
        out
    }

    #[test]
    fn two_documents_run_concurrently_and_same_doc_is_rejected() {
        let mut engine = AiEditEngine {
            jobs: Vec::new(),
            settings: AiSettings::default(),
        };
        assert!(engine.spawn_job(11, 2, 3, AiProvider::Gemini, false, || { ok_result(2, 3) }));
        assert!(engine.spawn_job(22, 4, 5, AiProvider::OpenAi, true, || { ok_result(4, 5) }));
        assert!(!engine.spawn_job(11, 1, 1, AiProvider::Gemini, false, || { ok_result(1, 1) }));
        assert!(engine.doc_running(11));
        assert!(engine.doc_running(22));

        let mut done = drain_with_deadline(&mut engine, 2);
        done.sort_by_key(|job| job.doc_id);
        assert_eq!(done.len(), 2);
        assert_eq!(done[0].doc_id, 11);
        assert_eq!(done[1].doc_id, 22);
        assert!(done[1].output_new_file);
        assert!(!engine.has_jobs());
    }

    #[test]
    fn abandoned_job_stops_blocking_doc_and_is_marked_for_discard() {
        let mut engine = AiEditEngine {
            jobs: Vec::new(),
            settings: AiSettings::default(),
        };
        assert!(engine.spawn_job(7, 1, 1, AiProvider::Gemini, false, || { ok_result(1, 1) }));
        assert!(engine.abandon_doc_job(7));
        assert!(!engine.doc_running(7));

        let done = drain_with_deadline(&mut engine, 1);
        assert_eq!(done.len(), 1);
        assert!(done[0].abandoned);
    }

    #[test]
    fn api_job_cap_rejects_fourth_worker() {
        let mut engine = AiEditEngine {
            jobs: Vec::new(),
            settings: AiSettings::default(),
        };
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(MAX_API_JOBS + 1));
        for doc_id in 1..=MAX_API_JOBS as u32 {
            let barrier = barrier.clone();
            assert!(
                engine.spawn_job(doc_id, 1, 1, AiProvider::Gemini, false, move || {
                    barrier.wait();
                    ok_result(1, 1)
                },)
            );
        }
        assert!(!engine.spawn_job(99, 1, 1, AiProvider::Gemini, false, || { ok_result(1, 1) }));
        barrier.wait();
        assert_eq!(
            drain_with_deadline(&mut engine, MAX_API_JOBS).len(),
            MAX_API_JOBS
        );
    }

    #[test]
    fn disconnected_worker_is_reaped_as_error() {
        let mut engine = AiEditEngine {
            jobs: Vec::new(),
            settings: AiSettings::default(),
        };
        let (tx, rx) = mpsc::channel();
        drop(tx);
        engine.jobs.push(AiJob {
            doc_id: 9,
            width: 1,
            height: 1,
            provider: AiProvider::Gemini,
            started: Instant::now(),
            output_new_file: false,
            abandoned: false,
            rx,
        });

        let done = engine.poll_finished();
        assert_eq!(done.len(), 1);
        assert!(done[0].result.is_err());
        assert!(!engine.has_jobs());
    }
}
