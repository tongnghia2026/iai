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
// background thread with a shared progress status the UI can poll. Where the
// file is, and where it is fetched from, is the model store's (`model_store`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::model_store::{self, Slot};

type OrtSession = ort::session::Session;

/// Whether the cached LaMa session currently runs on the GPU (DirectML).
static LAMA_ON_GPU: AtomicBool = AtomicBool::new(false);
/// Set once DirectML inference has failed for LaMa this session (some ops, e.g.
/// the FFC blocks, are unsupported / OOM on DirectML). Future loads then go
/// straight to CPU instead of paying the failed-GPU cost again.
static LAMA_GPU_DISABLED: AtomicBool = AtomicBool::new(false);

const DIM: u32 = 512;

/// The download's size, for messages ("~200 MB"), from the model table.
fn size_hint() -> String {
    format!(
        "~{}",
        model_store::fetch::size_text(Slot::Lama.model().total_bytes())
    )
}

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

fn status_cell() -> &'static parking_lot::Mutex<LamaStatus> {
    static S: OnceLock<parking_lot::Mutex<LamaStatus>> = OnceLock::new();
    S.get_or_init(|| {
        parking_lot::Mutex::new(if model_path().exists() {
            LamaStatus::Ready
        } else {
            LamaStatus::Absent
        })
    })
}

fn set_status(s: LamaStatus) {
    forget_available();
    *status_cell().lock() = s;
}

pub fn status() -> LamaStatus {
    status_cell().lock().clone()
}

/// True once the model file exists on disk (it may still need loading on first use).
pub fn is_available() -> bool {
    // Asked by the window every frame: the disk is asked at most every few
    // seconds. A busy disk once held the window 1.8 s on this question.
    let now = std::time::Instant::now();
    let mut kept = available_cell().lock();
    match *kept {
        Some((at, yes)) if now.duration_since(at) < AVAILABLE_KEPT => yes,
        _ => {
            let yes = model_path().exists();
            *kept = Some((now, yes));
            yes
        }
    }
}

/// How long the answer of [`is_available`] stands without asking the disk.
const AVAILABLE_KEPT: std::time::Duration = std::time::Duration::from_secs(5);

fn available_cell() -> &'static parking_lot::Mutex<Option<(std::time::Instant, bool)>> {
    static KEPT: OnceLock<parking_lot::Mutex<Option<(std::time::Instant, bool)>>> = OnceLock::new();
    KEPT.get_or_init(|| parking_lot::Mutex::new(None))
}

/// The model came or went: ask the disk again next time.
fn forget_available() {
    *available_cell().lock() = None;
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
            "Đang tải model AI inpainting ({})… {:.0}%",
            size_hint(),
            progress * 100.0
        )),
        LamaStatus::Error(e) => Some(format!("Lỗi model AI inpainting: {e}")),
        _ => None,
    }
}

/// Where the model is on this machine (in any model folder, under its own
/// name or, after a scan, any other), or where it is put when fetched.
pub fn model_path() -> PathBuf {
    model_store::locate(Slot::Lama).unwrap_or_else(|| model_store::install_path(Slot::Lama))
}

/// Marker left next to the model once LaMa has failed on this machine's GPU,
/// so later sessions go straight to the CPU instead of paying the failed
/// DirectML attempt (seconds) again. It holds the adapter name: a different
/// GPU gets tried afresh.
fn gpu_failed_marker() -> PathBuf {
    model_path().with_extension("gpu-failed")
}

fn adapter_name() -> String {
    iai_platform::hw::gpu().map_or_else(String::new, |g| g.name.clone())
}

fn gpu_known_to_fail() -> bool {
    std::fs::read_to_string(gpu_failed_marker()).is_ok_and(|s| s.trim() == adapter_name())
}

/// Kick off a background download if the model is absent (or a previous attempt
/// errored). No-op if it's already downloading or ready.
pub fn ensure_downloading() {
    {
        let g = status_cell().lock();
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

/// Fetch the model from the first of its sources in the author's list that
/// gives the right file (resumed where a last try stopped), reporting the
/// share done to the status.
fn download_blocking() -> Result<(), String> {
    let progress = |done: u64, total: u64| {
        if total > 0 {
            set_status(LamaStatus::Downloading {
                progress: done as f32 / total as f32,
            });
        }
    };
    let cancel = AtomicBool::new(false);
    model_store::fetch::download(Slot::Lama, &progress, &cancel)
        .map(|_| ())
        .map_err(|e| e.text())
}

fn session_cell() -> &'static Mutex<Option<OrtSession>> {
    static SESS: OnceLock<Mutex<Option<OrtSession>>> = OnceLock::new();
    SESS.get_or_init(|| Mutex::new(None))
}

/// Build the cached session: GPU (DirectML) when a real adapter is present
/// and DirectML is not known to fail for LaMa here; the helper handles
/// build-time fallback, and `inpaint` handles run-time fallback.
fn load_session(slot: &mut Option<OrtSession>) -> bool {
    let prefer_gpu = crate::ai::ort_ep::prefer_gpu()
        && !LAMA_GPU_DISABLED.load(Ordering::Relaxed)
        && !gpu_known_to_fail();
    match crate::ai::ort_ep::build_session(&model_path(), prefer_gpu) {
        Ok((s, gpu)) => {
            *slot = Some(s);
            LAMA_ON_GPU.store(gpu, Ordering::Relaxed);
            true
        }
        Err(e) => {
            set_status(LamaStatus::Error(e));
            false
        }
    }
}

/// Load the model in the background ahead of the first inpaint (the Repair
/// Brush asks for this when it is picked), so the first AI heal waits only
/// for the inference. No-op when the model is absent or already loading.
pub fn preload() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if !is_available() || STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("lama-preload".into())
        .spawn(|| {
            if let Ok(mut guard) = session_cell().lock()
                && guard.is_none()
            {
                load_session(&mut guard);
            }
        });
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
    if any { Some((x0, y0, x1, y1)) } else { None }
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

    // One inference on a given session. Input tensors are rebuilt from the pixel
    // buffers each attempt so a GPU failure can be retried on a fresh CPU session.
    let run_on = |sess: &mut OrtSession| -> Result<Vec<f32>, String> {
        let img = ort::value::Tensor::<f32>::from_array((
            [1i64, 3i64, DIM as i64, DIM as i64],
            chw.clone(),
        ))
        .map_err(|e| format!("input tensor: {e}"))?;
        let msk = ort::value::Tensor::<f32>::from_array((
            [1i64, 1i64, DIM as i64, DIM as i64],
            mbuf.clone(),
        ))
        .map_err(|e| format!("mask tensor: {e}"))?;
        let outputs = sess
            .run(ort::inputs!["image" => img, "mask" => msk])
            .map_err(|e| format!("inference: {e}"))?;
        let (_, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("extract: {e}"))?;
        Ok(data.to_vec())
    };

    let out_data: Vec<f32> = {
        let mut guard = match session_cell().lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if guard.is_none() && !load_session(&mut guard) {
            return false;
        }
        let Some(session) = guard.as_mut() else {
            return false;
        };
        match run_on(session) {
            Ok(data) => data,
            Err(e) => {
                // A GPU run failed (unsupported op / OOM). Disable the GPU for
                // LaMa, rebuild a clean CPU session, and retry once.
                if LAMA_ON_GPU.load(Ordering::Relaxed) {
                    LAMA_GPU_DISABLED.store(true, Ordering::Relaxed);
                    let _ = std::fs::write(gpu_failed_marker(), adapter_name());
                    let path = model_path();
                    match crate::ai::ort_ep::build_session(&path, false) {
                        Ok((s, _)) => {
                            *guard = Some(s);
                            LAMA_ON_GPU.store(false, Ordering::Relaxed);
                        }
                        Err(be) => {
                            set_status(LamaStatus::Error(be));
                            return false;
                        }
                    }
                    let rerun = match guard.as_mut() {
                        Some(session) => run_on(session),
                        None => return false,
                    };
                    match rerun {
                        Ok(data) => data,
                        Err(e2) => {
                            set_status(LamaStatus::Error(format!("inference: {e2}")));
                            return false;
                        }
                    }
                } else {
                    set_status(LamaStatus::Error(format!("inference: {e}")));
                    return false;
                }
            }
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
