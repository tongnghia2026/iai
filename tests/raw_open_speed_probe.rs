//! Where does opening a RAW spend its time, and does a speed-up change the
//! pixels? Times the embedded-preview extract and the full decode (in a pool
//! of the size the app uses) for every RAW in `IAI_RAW_CORPUS`, and prints a
//! fingerprint of the scene master plus the rendered tiles so optimisations
//! can be checked bit-for-bit against the previous build.
//!
//! ```text
//! IAI_RAW_CORPUS="C:\\path\\to\\raws" \
//!   cargo test --release --test raw_open_speed_probe -- --ignored --nocapture
//! ```
//!
//! Optional: `IAI_RAW_SPEED_FILTER` (substring), `IAI_RAW_SPEED_THREADS`,
//! `IAI_RAW_SPEED_REPEAT`.

use std::path::PathBuf;
use std::time::Instant;

use iai::formats::raw::{RawDecodeControl, RawImporter};
use iai::formats::Importer;

fn fnv1a(hash: &mut u64, words: &[u16]) {
    for &word in words {
        *hash ^= word as u64;
        *hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
}

fn fingerprint(canvas: &iai::core::canvas::Canvas) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    if let Some(scene) = &canvas.develop_source {
        fnv1a(&mut hash, &scene.half);
    }
    fnv1a(&mut hash, &canvas.layer_stack.layers[0].tiles.flatten16());
    hash
}

#[test]
#[ignore = "requires a local RAW corpus via IAI_RAW_CORPUS; slow"]
fn raw_open_speed() {
    let Some(dir) = std::env::var("IAI_RAW_CORPUS").ok().map(PathBuf::from) else {
        eprintln!("IAI_RAW_CORPUS not set; skipping");
        return;
    };
    iai::core::hw::opt_out_of_power_throttling();
    let filter = std::env::var("IAI_RAW_SPEED_FILTER").unwrap_or_default();
    let threads: usize = std::env::var("IAI_RAW_SPEED_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, usize::from)
                .div_ceil(2)
        });
    let repeat: usize = std::env::var("IAI_RAW_SPEED_REPEAT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let importer = RawImporter;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && importer.can_import(p))
        .filter(|p| filter.is_empty() || p.to_string_lossy().contains(&filter))
        .collect();
    entries.sort();
    println!("threads={threads}");
    for path in entries
        .iter()
        .flat_map(|p| std::iter::repeat(p).take(repeat))
    {
        let name: String = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .chars()
            .take(40)
            .collect();
        // IAI_RAW_SPEED_COLD=1 skips the preview worker, like the first image
        // of an open, so the decode measures the camera JPEG itself.
        let t = Instant::now();
        if std::env::var_os("IAI_RAW_SPEED_COLD").is_none() {
            drop(iai::formats::raw_preview::extract(path));
        }
        let preview_ms = t.elapsed().as_millis();
        let t = Instant::now();
        let draft_at = std::sync::Mutex::new(None);
        let sink = |draft: iai::core::canvas::Canvas| {
            *draft_at.lock().unwrap() = Some((t.elapsed().as_millis(), draft.width, draft.height));
        };
        let control = RawDecodeControl {
            cancel: None,
            draft: Some(&sink),
        };
        let canvas = pool
            .install(|| iai::formats::raw::decode_raw_controlled(path, control))
            .unwrap();
        let decode_ms = t.elapsed().as_millis();
        let draft = draft_at
            .into_inner()
            .unwrap()
            .map_or("none".to_string(), |(ms, w, h)| format!("{w}x{h} @{ms} ms"));
        let mp = canvas.width as f64 * canvas.height as f64 / 1e6;
        let hash = fingerprint(&canvas);
        let t = Instant::now();
        let thumb = canvas.downscaled_thumbnail(2048);
        let thumb_ms = t.elapsed().as_millis();
        drop(thumb);
        let t = Instant::now();
        let spill = iai::core::raw_spill::RawSpill::write_now(&canvas).unwrap();
        let write_ms = t.elapsed().as_millis();
        drop(canvas);
        let t = Instant::now();
        let restored = spill.restore().unwrap();
        let restore_ms = t.elapsed().as_millis();
        assert_eq!(fingerprint(&restored), hash, "{name}: spill round trip");
        println!(
            "{name:<40} {mp:>5.1} MP  decode {decode_ms:>6} ms  draft {draft}  thumb {thumb_ms} ms  spill w {write_ms} r {restore_ms} ms  preview {preview_ms} ms  hash {hash:016x}"
        );
    }
}
