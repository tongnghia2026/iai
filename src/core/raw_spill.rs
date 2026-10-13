//! Disk spill for decoded RAW Develop images.
//!
//! A multi-RAW Develop session keeps only the active image (plus one recent
//! one) at full resolution. Instead of throwing a parked image's decode away,
//! its scene master and default-look pixels are written to a private temp
//! file, so returning to it is a sub-second read rather than a full demosaic.
//! The file is opened delete-on-close: the OS removes it when the handle is
//! dropped, including after a crash.

use crate::core::canvas::{Canvas, CanvasMetadata, IccProfile};
use crate::core::develop_scene::SceneSource;
use rayon::prelude::*;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// f16 1.0 — the alpha of every opaque scene-master pixel.
const SCENE_OPAQUE: u16 = 0x3c00;
/// Rows converted and written per I/O call.
const ROWS_PER_CHUNK: usize = 64;

struct SpillFile {
    file: File,
    width: u32,
    height: u32,
    scene_channels: usize,
    look_channels: usize,
    /// The scene master without its pixels.
    scene: SceneSource,
    icc_profile: IccProfile,
    metadata: CanvasMetadata,
}

enum SpillState {
    Writing,
    Ready(Box<SpillFile>),
    Failed,
}

/// One parked RAW image on disk. Shared between the document that owns it
/// and the worker that writes or restores it.
pub struct RawSpill {
    state: Mutex<SpillState>,
    written: Condvar,
    bytes: AtomicU64,
}

impl RawSpill {
    /// Disk bytes this spill occupies, or will once its write finishes.
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// The write failed; restoring would fail too.
    pub fn is_failed(&self) -> bool {
        self.state
            .lock()
            .map_or(true, |state| matches!(*state, SpillState::Failed))
    }

    /// Whether `canvas` is a single-layer RAW decode this module can store.
    pub fn can_spill(canvas: &Canvas) -> bool {
        let Some(scene) = canvas.develop_source.as_ref() else {
            return false;
        };
        let layers = &canvas.layer_stack.layers;
        scene.alpha.is_none()
            && scene.width == canvas.width
            && scene.height == canvas.height
            && scene.half.len() == canvas.width as usize * canvas.height as usize * 4
            && layers.len() == 1
            && layers[0].is_raster()
            && layers[0].tiles.has_hdr()
    }

    /// Write `canvas` on a background thread and return the handle at once;
    /// the canvas is dropped when the write completes. `None` when the canvas
    /// cannot be spilled.
    pub fn spawn(canvas: Canvas) -> Option<Arc<RawSpill>> {
        if !Self::can_spill(&canvas) {
            return None;
        }
        let spill = Arc::new(Self::pending(&canvas));
        let worker = Arc::clone(&spill);
        std::thread::Builder::new()
            .name("iai-raw-spill".to_string())
            .spawn(move || {
                // Nobody holds the handle any more (document closed): skip.
                let result = if Arc::strong_count(&worker) > 1 {
                    write_spill(&canvas)
                } else {
                    Err("spill abandoned".to_string())
                };
                drop(canvas);
                worker.finish(result);
            })
            .ok()?;
        Some(spill)
    }

    /// Write `canvas` on the calling thread.
    pub fn write_now(canvas: &Canvas) -> Result<Arc<RawSpill>, String> {
        if !Self::can_spill(canvas) {
            return Err("canvas is not a spillable RAW decode".to_string());
        }
        let spill = Self::pending(canvas);
        spill.finish(write_spill(canvas));
        if spill.is_failed() {
            return Err("RAW spill write failed".to_string());
        }
        Ok(Arc::new(spill))
    }

    /// Rebuild the full canvas (scene master attached). Blocks while the
    /// initial write is still running.
    pub fn restore(&self) -> Result<Canvas, String> {
        let mut state = self.state.lock().map_err(|_| "spill lock poisoned")?;
        while matches!(*state, SpillState::Writing) {
            state = self
                .written
                .wait(state)
                .map_err(|_| "spill lock poisoned")?;
        }
        match &mut *state {
            SpillState::Ready(file) => restore_spill(file).map_err(|e| e.to_string()),
            _ => Err("RAW spill unavailable".to_string()),
        }
    }

    fn pending(canvas: &Canvas) -> Self {
        let pixels = canvas.width as u64 * canvas.height as u64;
        Self {
            state: Mutex::new(SpillState::Writing),
            written: Condvar::new(),
            // 3 scene + 3 look channels of u16; refined once written.
            bytes: AtomicU64::new(pixels * 12),
        }
    }

    fn finish(&self, result: Result<SpillFile, String>) {
        let next = match result {
            Ok(file) => {
                let pixels = file.width as u64 * file.height as u64;
                let channels = (file.scene_channels + file.look_channels) as u64;
                self.bytes.store(pixels * channels * 2, Ordering::Relaxed);
                SpillState::Ready(Box::new(file))
            }
            Err(_) => {
                self.bytes.store(0, Ordering::Relaxed);
                SpillState::Failed
            }
        };
        if let Ok(mut state) = self.state.lock() {
            *state = next;
        }
        self.written.notify_all();
    }
}

fn create_spill_file() -> std::io::Result<File> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "iai-raw-{}-{}.spill",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let path = std::env::temp_dir().join(name);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        options
            .share_mode(0)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
        options.open(&path)
    }
    #[cfg(not(windows))]
    {
        let file = options.open(&path)?;
        let _ = std::fs::remove_file(&path);
        Ok(file)
    }
}

fn write_spill(canvas: &Canvas) -> Result<SpillFile, String> {
    let scene = canvas
        .develop_source
        .as_ref()
        .ok_or("RAW spill needs a scene master")?;
    let layer = &canvas.layer_stack.layers[0];
    let (w, h) = (canvas.width as usize, canvas.height as usize);
    let scene_channels = if scene
        .half
        .par_chunks_exact(4)
        .all(|px| px[3] == SCENE_OPAQUE)
    {
        3
    } else {
        4
    };
    let look_channels = if layer.is_background { 3 } else { 4 };
    let mut file = create_spill_file().map_err(|e| e.to_string())?;
    let mut buf: Vec<u16> = Vec::with_capacity(ROWS_PER_CHUNK * w * 4);
    for chunk in scene.half.chunks(ROWS_PER_CHUNK * w * 4) {
        pack_rows(chunk, scene_channels, &mut buf);
        file.write_all(bytemuck::cast_slice(&buf))
            .map_err(|e| e.to_string())?;
    }
    let mut band = vec![0u16; ROWS_PER_CHUNK * w * 4];
    let mut y = 0;
    while y < h {
        let rows = ROWS_PER_CHUNK.min(h - y);
        let band = &mut band[..rows * w * 4];
        layer
            .tiles
            .flatten16_region_into(0, y as u32, w as u32, rows as u32, band);
        if look_channels == 3 && band.chunks_exact(4).any(|px| px[3] != u16::MAX) {
            return Err("RAW look has transparency".to_string());
        }
        pack_rows(band, look_channels, &mut buf);
        file.write_all(bytemuck::cast_slice(&buf))
            .map_err(|e| e.to_string())?;
        y += rows;
    }
    file.flush().map_err(|e| e.to_string())?;
    Ok(SpillFile {
        file,
        width: canvas.width,
        height: canvas.height,
        scene_channels,
        look_channels,
        scene: SceneSource {
            width: scene.width,
            height: scene.height,
            half: Vec::new(),
            alpha: None,
            look: scene.look,
            color_pipeline: scene.color_pipeline,
            camera_profile: scene.camera_profile.clone(),
            as_shot_white_balance: scene.as_shot_white_balance,
            camera_rgb_curve: scene.camera_rgb_curve.clone(),
        },
        icc_profile: canvas.icc_profile.clone(),
        metadata: canvas.metadata.clone(),
    })
}

fn pack_rows(rgba: &[u16], channels: usize, out: &mut Vec<u16>) {
    out.clear();
    if channels == 4 {
        out.extend_from_slice(rgba);
    } else {
        for px in rgba.chunks_exact(4) {
            out.extend_from_slice(&px[..3]);
        }
    }
}

/// Read `channels`-per-pixel rows back into an RGBA buffer, filling a dropped
/// alpha channel with `alpha`.
fn read_rgba(
    file: &mut File,
    pixels: usize,
    width: usize,
    channels: usize,
    alpha: u16,
) -> std::io::Result<Vec<u16>> {
    let mut rgba = vec![alpha; pixels * 4];
    if channels == 4 {
        file.read_exact(bytemuck::cast_slice_mut(&mut rgba))?;
        return Ok(rgba);
    }
    let mut buf = vec![0u16; ROWS_PER_CHUNK * width * 3];
    for chunk in rgba.chunks_mut(ROWS_PER_CHUNK * width * 4) {
        let packed = &mut buf[..chunk.len() / 4 * 3];
        file.read_exact(bytemuck::cast_slice_mut(packed))?;
        for (dst, src) in chunk.chunks_exact_mut(4).zip(packed.chunks_exact(3)) {
            dst[..3].copy_from_slice(src);
        }
    }
    Ok(rgba)
}

fn restore_spill(spill: &mut SpillFile) -> std::io::Result<Canvas> {
    let (w, h) = (spill.width as usize, spill.height as usize);
    spill.file.seek(SeekFrom::Start(0))?;
    let half = read_rgba(
        &mut spill.file,
        w * h,
        w,
        spill.scene_channels,
        SCENE_OPAQUE,
    )?;
    let px16 = read_rgba(&mut spill.file, w * h, w, spill.look_channels, u16::MAX)?;
    let mut canvas = Canvas::from_rgba16(px16, spill.width, spill.height);
    let scene = SceneSource {
        width: spill.scene.width,
        height: spill.scene.height,
        half,
        alpha: None,
        look: spill.scene.look,
        color_pipeline: spill.scene.color_pipeline,
        camera_profile: spill.scene.camera_profile.clone(),
        as_shot_white_balance: spill.scene.as_shot_white_balance,
        camera_rgb_curve: spill.scene.camera_rgb_curve.clone(),
    };
    canvas.develop_source = Some(Arc::new(scene));
    canvas.icc_profile = spill.icc_profile.clone();
    canvas.metadata = spill.metadata.clone();
    Ok(canvas)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::develop_scene::f32_to_f16_bits;

    fn raw_like_canvas(w: u32, h: u32) -> Canvas {
        let n = (w * h) as usize;
        let mut half = vec![0u16; n * 4];
        let mut px16 = vec![0u16; n * 4];
        for i in 0..n {
            for c in 0..3 {
                half[i * 4 + c] = f32_to_f16_bits((i * 3 + c) as f32 * 0.001 - 0.2);
                px16[i * 4 + c] = ((i * 7 + c * 13) % 65536) as u16;
            }
            half[i * 4 + 3] = SCENE_OPAQUE;
            px16[i * 4 + 3] = u16::MAX;
        }
        let mut canvas = Canvas::from_rgba16(px16, w, h);
        let mut scene = SceneSource::new(w, h);
        scene.half = half;
        canvas.develop_source = Some(Arc::new(scene));
        canvas.metadata.source_profile = "Test Camera".to_string();
        canvas
    }

    #[test]
    fn spill_round_trips_scene_and_look_exactly() {
        // Odd sizes cover partial tiles and a partial final row chunk.
        let canvas = raw_like_canvas(301, 133);
        let spill = RawSpill::write_now(&canvas).expect("spill");
        assert_eq!(spill.bytes(), 301 * 133 * 12);
        let restored = spill.restore().expect("restore");
        assert_eq!((restored.width, restored.height), (301, 133));
        let a = canvas.develop_source.as_ref().unwrap();
        let b = restored.develop_source.as_ref().unwrap();
        assert_eq!(a.half, b.half);
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.flatten16(),
            restored.layer_stack.layers[0].tiles.flatten16()
        );
        assert_eq!(restored.metadata.source_profile, "Test Camera");
        // A second restore reads the same file again.
        let again = spill.restore().expect("restore twice");
        assert_eq!(again.develop_source.as_ref().unwrap().half, a.half);
    }

    #[test]
    fn background_spill_restores_after_its_write() {
        let canvas = raw_like_canvas(64, 48);
        let expected = canvas.develop_source.as_ref().unwrap().half.clone();
        let spill = RawSpill::spawn(canvas).expect("spillable");
        let restored = spill.restore().expect("restore waits for the write");
        assert_eq!(restored.develop_source.as_ref().unwrap().half, expected);
    }

    #[test]
    fn non_raw_canvas_is_not_spillable() {
        let canvas = Canvas::new(16, 16);
        assert!(!RawSpill::can_spill(&canvas));
        assert!(RawSpill::spawn(canvas).is_none());
    }
}
