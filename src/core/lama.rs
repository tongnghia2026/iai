// LaMa (Large-Mask) inpainting via a local ONNX model — the "AI" backend for
// Smart Fill. When the model isn't present the caller falls back to the
// CPU PatchMatch path, so this is purely an opt-in quality upgrade.
//
// Model: Carve/LaMa-ONNX `lama_fp32.onnx` (~200 MB, opset 17). Fixed 512×512:
//   input  "image" : [1,3,512,512] f32, RGB, value/255, no mean/std
//   input  "mask"  : [1,1,512,512] f32, binarised (>0 → 1), 1 = pixel to fill
//   output         : [1,3,512,512] f32, RGB already in 0..255
//
// Pipeline per call: crop to the hole bbox + a context band, resize that crop to
// 512², run the net, resize the result back, and write ONLY the original hole
// pixels (source pixels are never touched — same contract as PatchMatch `fill`).
//
// The ~200 MB session is cached process-wide so it loads once. Download runs on a
// background thread with a shared progress status the UI can poll.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

type OrtSession = ort::session::Session;

const MODEL_FILE: &str = "lama_fp32.onnx";
const MODEL_URL: &str = "https://huggingface.co/Carve/LaMa-ONNX/resolve/main/lama_fp32.onnx";
const MODEL_SIZE_HINT: &str = "~200 MB";
const DIM: u32 = 512;

#[derive(Clone, Debug)]
pub enum LamaStatus {
    /// Model file not on disk yet.
    Absent,
    Downloading {
        progress: f32,
    },
    Ready,
    Error(String),
}

fn status_cell() -> &'static Mutex<LamaStatus> {
    static S: OnceLock<Mutex<LamaStatus>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(if model_path().exists() {
            LamaStatus::Ready
        } else {
            LamaStatus::Absent
        })
    })
}

fn set_status(s: LamaStatus) {
    if let Ok(mut g) = status_cell().lock() {
        *g = s;
    }
}

pub fn status() -> LamaStatus {
    status_cell()
        .lock()
        .map(|g| g.clone())
        .unwrap_or(LamaStatus::Absent)
}

/// True once the model file exists on disk (it may still need loading on first use).
pub fn is_available() -> bool {
    model_path().exists()
}

/// True while a background download is in progress (used to keep the UI repainting
/// so the progress status stays live).
pub fn is_downloading() -> bool {
    matches!(status(), LamaStatus::Downloading { .. })
}

/// A one-line status string for the status bar while downloading / on error, or
/// `None` when there's nothing noteworthy to show.
pub fn status_text() -> Option<String> {
    match status() {
        LamaStatus::Downloading { progress } => Some(format!(
            "Đang tải model AI inpainting ({MODEL_SIZE_HINT})… {:.0}%",
            progress * 100.0
        )),
        LamaStatus::Error(e) => Some(format!("Lỗi model AI inpainting: {e}")),
        _ => None,
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

pub fn model_path() -> PathBuf {
    models_dir().join(MODEL_FILE)
}

/// Kick off a background download if the model is absent (or a previous attempt
/// errored). No-op if it's already downloading or ready.
pub fn ensure_downloading() {
    {
        let g = status_cell().lock().unwrap();
        match *g {
            LamaStatus::Absent | LamaStatus::Error(_) => {}
            _ => return,
        }
    }
    set_status(LamaStatus::Downloading { progress: 0.0 });
    std::thread::spawn(|| match download_blocking() {
        Ok(()) => set_status(LamaStatus::Ready),
        Err(e) => set_status(LamaStatus::Error(e)),
    });
}

fn download_blocking() -> Result<(), String> {
    let path = model_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1800))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;

    let mut req = client.get(MODEL_URL);
    if let Ok(token) = std::env::var("HF_TOKEN").or_else(|_| std::env::var("HUGGINGFACE_TOKEN")) {
        if !token.trim().is_empty() {
            req = req.bearer_auth(token.trim());
        }
    }

    let resp = req
        .send()
        .map_err(|e| format!("download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("download failed: {e}"))?;

    let total = resp.content_length().unwrap_or(0);
    let tmp = path.with_extension("onnx.part");

    use std::io::{Read, Write};
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("create file: {e}"))?;
    let mut stream = resp;
    let mut downloaded: u64 = 0;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                drop(file);
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("read error: {e}"));
            }
        };
        if let Err(e) = file.write_all(&buf[..n]) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("write: {e}"));
        }
        downloaded += n as u64;
        if total > 0 {
            set_status(LamaStatus::Downloading {
                progress: downloaded as f32 / total as f32,
            });
        }
    }
    file.flush().map_err(|e| format!("flush: {e}"))?;
    drop(file);

    if total > 0 && downloaded != total {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("incomplete download ({downloaded}/{total} bytes)"));
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

fn session_cell() -> &'static Mutex<Option<OrtSession>> {
    static SESS: OnceLock<Mutex<Option<OrtSession>>> = OnceLock::new();
    SESS.get_or_init(|| Mutex::new(None))
}

fn hole_bbox(hole: &[bool], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    let mut any = false;
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            if hole[row + x] {
                any = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if any {
        Some((x0, y0, x1, y1))
    } else {
        None
    }
}

/// Inpaint the `hole` pixels of a straight-alpha RGBA8 buffer (`w×h`) with LaMa.
/// Returns `true` only on full success (buffer modified); `false` — including
/// "model not downloaded" or any inference error — leaves the buffer untouched so
/// the caller can fall back to PatchMatch.
pub fn inpaint(rgba: &mut [u8], w: usize, h: usize, hole: &[bool]) -> bool {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 || hole.len() < w * h {
        return false;
    }
    if !is_available() {
        return false;
    }
    let Some((hx0, hy0, hx1, hy1)) = hole_bbox(hole, w, h) else {
        return false;
    };

    let hole_w = hx1 - hx0;
    let hole_h = hy1 - hy0;
    let margin = (hole_w.max(hole_h) / 2).clamp(16, 256);
    let rx0 = hx0.saturating_sub(margin);
    let ry0 = hy0.saturating_sub(margin);
    let rx1 = (hx1 + margin).min(w);
    let ry1 = (hy1 + margin).min(h);
    let rw = rx1 - rx0;
    let rh = ry1 - ry0;
    if rw == 0 || rh == 0 {
        return false;
    }

    let mut crop = image::RgbaImage::new(rw as u32, rh as u32);
    let mut mask_g = image::GrayImage::new(rw as u32, rh as u32);
    for y in 0..rh {
        for x in 0..rw {
            let si = ((ry0 + y) * w + (rx0 + x)) * 4;
            crop.put_pixel(
                x as u32,
                y as u32,
                image::Rgba([rgba[si], rgba[si + 1], rgba[si + 2], rgba[si + 3]]),
            );
            let hv = if hole[(ry0 + y) * w + (rx0 + x)] {
                255u8
            } else {
                0
            };
            mask_g.put_pixel(x as u32, y as u32, image::Luma([hv]));
        }
    }
    let img512 = image::imageops::resize(&crop, DIM, DIM, image::imageops::FilterType::Lanczos3);
    let mask512 = image::imageops::resize(&mask_g, DIM, DIM, image::imageops::FilterType::Triangle);

    let n = (DIM * DIM) as usize;
    let mut chw = vec![0f32; 3 * n];
    for (i, px) in img512.pixels().enumerate() {
        chw[i] = px.0[0] as f32 / 255.0;
        chw[n + i] = px.0[1] as f32 / 255.0;
        chw[2 * n + i] = px.0[2] as f32 / 255.0;
    }
    let mut mbuf = vec![0f32; n];
    for (i, px) in mask512.pixels().enumerate() {
        mbuf[i] = if px.0[0] > 0 { 1.0 } else { 0.0 };
    }

    let img_tensor =
        match ort::value::Tensor::<f32>::from_array(([1i64, 3i64, DIM as i64, DIM as i64], chw)) {
            Ok(t) => t,
            Err(_) => return false,
        };
    let mask_tensor =
        match ort::value::Tensor::<f32>::from_array(([1i64, 1i64, DIM as i64, DIM as i64], mbuf)) {
            Ok(t) => t,
            Err(_) => return false,
        };

    let out_data: Vec<f32> = {
        let mut guard = match session_cell().lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if guard.is_none() {
            let path = model_path();
            let built = (|| -> Result<OrtSession, String> {
                let mut b = OrtSession::builder().map_err(|e| format!("ORT builder: {e}"))?;
                b.commit_from_file(&path)
                    .map_err(|e| format!("ORT load: {e}"))
            })();
            match built {
                Ok(s) => *guard = Some(s),
                Err(e) => {
                    set_status(LamaStatus::Error(e));
                    return false;
                }
            }
        }
        let sess = guard.as_mut().unwrap();
        let outputs = match sess.run(ort::inputs!["image" => img_tensor, "mask" => mask_tensor]) {
            Ok(o) => o,
            Err(e) => {
                set_status(LamaStatus::Error(format!("inference: {e}")));
                return false;
            }
        };
        match outputs[0].try_extract_tensor::<f32>() {
            Ok((_, data)) => data.to_vec(),
            Err(_) => return false,
        }
    };
    if out_data.len() < 3 * n {
        return false;
    }

    let maxv = out_data[..3 * n].iter().copied().fold(0f32, f32::max);
    let scale = if maxv <= 1.5 { 255.0 } else { 1.0 };
    let mut out512 = image::RgbImage::new(DIM, DIM);
    for i in 0..n {
        let r = (out_data[i] * scale).clamp(0.0, 255.0) as u8;
        let g = (out_data[n + i] * scale).clamp(0.0, 255.0) as u8;
        let b = (out_data[2 * n + i] * scale).clamp(0.0, 255.0) as u8;
        out512.put_pixel((i as u32) % DIM, (i as u32) / DIM, image::Rgb([r, g, b]));
    }
    let out_crop = image::imageops::resize(
        &out512,
        rw as u32,
        rh as u32,
        image::imageops::FilterType::Lanczos3,
    );

    for y in 0..rh {
        for x in 0..rw {
            if !hole[(ry0 + y) * w + (rx0 + x)] {
                continue;
            }
            let p = out_crop.get_pixel(x as u32, y as u32);
            let di = ((ry0 + y) * w + (rx0 + x)) * 4;
            rgba[di] = p.0[0];
            rgba[di + 1] = p.0[1];
            rgba[di + 2] = p.0[2];
            rgba[di + 3] = 255;
        }
    }
    true
}
