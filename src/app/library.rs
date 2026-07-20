//! The lightweight RAW/image library (Track B): a recent-files catalog and a
//! disk-backed thumbnail cache.
//!
//! "Light" means a fast browser over recently opened files — no non-destructive
//! recipes, no batch sync. Edits still bake on commit exactly as before. The
//! catalog is a small JSON list under `%APPDATA%/IAI/catalog.json`; thumbnails
//! are 256px JPEGs under `%APPDATA%/IAI/thumbs/`, generated off the UI thread
//! from a RAW's embedded preview (A1) or an ordinary decode.

use super::autosave::{ensure_data_child_dir, iai_data_dir};
use super::state::App;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

/// Longest side (px) of a generated thumbnail.
const THUMB_MAX: u32 = 256;
/// Cap the number of recent entries kept in the catalog.
const MAX_RECENT: usize = 24;
/// Cap the number of decoded thumbnail textures kept resident in RAM.
const MAX_THUMB_TEX: usize = 256;

/// One recent-files record, persisted in `catalog.json`.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RecentEntry {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Unix seconds of the last open, newest first in the list.
    pub last_opened: u64,
}

impl Default for RecentEntry {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            width: 0,
            height: 0,
            last_opened: 0,
        }
    }
}

/// The recent-files list. `serde(default)` keeps old files loadable.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Catalog {
    pub entries: Vec<RecentEntry>,
}

impl Catalog {
    /// Load the catalog from disk, or an empty one if it is missing/corrupt.
    pub fn load() -> Self {
        catalog_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist the catalog (best-effort).
    pub fn save(&self) {
        let Some(path) = catalog_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }

    /// Move `path` to the front of the recent list (most recent), replacing any
    /// existing record for it, and cap the list length. `now` is Unix seconds.
    pub fn record(&mut self, path: &Path, width: u32, height: u32, now: u64) {
        self.entries.retain(|e| e.path.as_path() != path);
        self.entries.insert(
            0,
            RecentEntry {
                path: path.to_path_buf(),
                width,
                height,
                last_opened: now,
            },
        );
        self.entries.truncate(MAX_RECENT);
    }

    pub fn recent(&self) -> &[RecentEntry] {
        &self.entries
    }
}

fn catalog_path() -> Option<PathBuf> {
    Some(iai_data_dir()?.join("catalog.json"))
}

fn thumbs_dir() -> Option<PathBuf> {
    ensure_data_child_dir("thumbs")
}

/// Stable on-disk key for a file's thumbnail: the path plus its modification
/// time, so editing a file in place invalidates its cached thumbnail.
fn thumb_key(path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    if let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) {
        if let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH) {
            d.as_secs().hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

fn thumb_cache_file(path: &Path) -> Option<PathBuf> {
    Some(thumbs_dir()?.join(format!("{}.jpg", thumb_key(path))))
}

fn color_image_from_rgba(img: &image::RgbaImage) -> egui::ColorImage {
    let (w, h) = img.dimensions();
    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw())
}

/// Decode a source image (a RAW's embedded preview, or a normal image) into an
/// RGBA `DynamicImage`. `None` for a missing/undecodable file.
fn load_source(path: &Path) -> Option<image::DynamicImage> {
    if crate::formats::raw::is_raw_path(path) {
        let preview = crate::formats::raw_preview::extract(path)?;
        let buf = image::RgbaImage::from_raw(preview.width, preview.height, preview.rgba)?;
        Some(image::DynamicImage::ImageRgba8(buf))
    } else {
        image::open(path).ok()
    }
}

/// Produce a thumbnail `ColorImage` for `path`: reuse the on-disk JPEG if it is
/// present, otherwise generate one from the source, cache it, and return it.
/// Runs on the worker thread — no egui/GPU access here.
fn produce_thumb(path: &Path) -> Option<egui::ColorImage> {
    let cache = thumb_cache_file(path);
    if let Some(cache) = &cache {
        if let Ok(bytes) = std::fs::read(cache) {
            if let Ok(img) = image::load_from_memory(&bytes) {
                return Some(color_image_from_rgba(&img.to_rgba8()));
            }
        }
    }
    let source = load_source(path)?;
    let thumb = source.thumbnail(THUMB_MAX, THUMB_MAX).to_rgba8();
    let color = color_image_from_rgba(&thumb);
    if let Some(cache) = &cache {
        // JPEG has no alpha; the thumbnails are opaque photos, so drop it.
        let _ = image::DynamicImage::ImageRgba8(thumb).to_rgb8().save(cache);
    }
    Some(color)
}

/// A disk-backed, off-thread thumbnail cache with a resident-texture LRU.
///
/// The UI asks for a path's thumbnail via [`request`](Self::request); a single
/// worker generates it (or loads the cached JPEG) and streams the pixels back;
/// [`poll`](Self::poll) uploads ready results to egui textures on the main
/// context. [`get`](Self::get) returns the texture once it is resident.
#[derive(Default)]
pub struct ThumbCache {
    tex: HashMap<PathBuf, egui::TextureHandle>,
    /// Insertion order of `tex`, for LRU eviction.
    order: VecDeque<PathBuf>,
    /// Paths whose generation is in flight, so a path is requested only once.
    requested: HashSet<PathBuf>,
    req_tx: Option<Sender<PathBuf>>,
    res_rx: Option<Receiver<(PathBuf, egui::ColorImage)>>,
}

impl ThumbCache {
    fn ensure_worker(&mut self) {
        if self.req_tx.is_some() {
            return;
        }
        let (req_tx, req_rx) = std::sync::mpsc::channel::<PathBuf>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<(PathBuf, egui::ColorImage)>();
        std::thread::spawn(move || {
            while let Ok(path) = req_rx.recv() {
                // A file that fails to decode simply never sends a result; it
                // stays flagged as requested so it is not retried in a loop.
                if let Some(img) = produce_thumb(&path) {
                    if res_tx.send((path, img)).is_err() {
                        break; // UI dropped the receiver (app closing).
                    }
                }
            }
        });
        self.req_tx = Some(req_tx);
        self.res_rx = Some(res_rx);
    }

    /// Queue a thumbnail for `path` unless it is already resident or in flight.
    pub fn request(&mut self, path: &Path) {
        if self.tex.contains_key(path) || self.requested.contains(path) {
            return;
        }
        self.ensure_worker();
        self.requested.insert(path.to_path_buf());
        if let Some(tx) = &self.req_tx {
            let _ = tx.send(path.to_path_buf());
        }
    }

    /// Upload any finished thumbnails to textures on `ctx`. Cheap when idle.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut ready: Vec<(PathBuf, egui::ColorImage)> = Vec::new();
        if let Some(rx) = self.res_rx.as_ref() {
            while let Ok(item) = rx.try_recv() {
                ready.push(item);
            }
        }
        for (path, img) in ready {
            let handle = ctx.load_texture(
                format!("library_thumb_{}", thumb_key(&path)),
                img,
                egui::TextureOptions::LINEAR,
            );
            self.requested.remove(&path);
            self.order.retain(|p| p != &path);
            self.order.push_back(path.clone());
            self.tex.insert(path, handle);
        }
        while self.tex.len() > MAX_THUMB_TEX {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.tex.remove(&oldest);
        }
    }

    pub fn get(&self, path: &Path) -> Option<&egui::TextureHandle> {
        self.tex.get(path)
    }
}

/// The library subsystem held by [`App`]: the recent-files catalog plus the
/// thumbnail cache.
pub struct LibraryShell {
    pub catalog: Catalog,
    pub thumbs: ThumbCache,
}

impl LibraryShell {
    pub fn new() -> Self {
        Self {
            catalog: Catalog::load(),
            thumbs: ThumbCache::default(),
        }
    }
}

impl App {
    /// Record a freshly opened file in the recent-files catalog (persisted) and
    /// prime its thumbnail. Called when a decode lands (see `poll_loads`).
    pub(in crate::app) fn record_recent(&mut self, path: &Path, width: u32, height: u32) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.lib.catalog.record(path, width, height, now);
        self.lib.catalog.save();
        self.lib.thumbs.request(path);
    }

    /// Build the welcome screen's recent-files view models, uploading any ready
    /// thumbnails and requesting the missing ones on the main egui context.
    /// Called from `collect_ui_data` while the welcome screen is showing.
    pub(in crate::app) fn welcome_recent_view(&mut self) -> Vec<crate::ui::RecentThumb> {
        let ctx = self.win.egui_ctx.clone();
        self.lib.thumbs.poll(&ctx);
        let entries: Vec<RecentEntry> = self.lib.catalog.recent().to_vec();
        entries
            .iter()
            .map(|e| {
                self.lib.thumbs.request(&e.path);
                let thumb = self
                    .lib
                    .thumbs
                    .get(&e.path)
                    .map(|t| (t.id(), t.size_vec2()));
                let name = e
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                crate::ui::RecentThumb {
                    path: e.path.clone(),
                    name,
                    dims: (e.width, e.height),
                    thumb,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_dedupes_and_orders_newest_first() {
        let mut cat = Catalog::default();
        cat.record(Path::new("/a.cr2"), 100, 50, 10);
        cat.record(Path::new("/b.jpg"), 20, 20, 20);
        cat.record(Path::new("/a.cr2"), 100, 50, 30); // re-open a
        let recent = cat.recent();
        assert_eq!(recent.len(), 2, "re-open must not duplicate");
        assert_eq!(recent[0].path.as_path(), Path::new("/a.cr2"));
        assert_eq!(recent[0].last_opened, 30);
        assert_eq!(recent[1].path.as_path(), Path::new("/b.jpg"));
    }

    #[test]
    fn record_caps_the_list() {
        let mut cat = Catalog::default();
        for i in 0..(MAX_RECENT + 8) {
            cat.record(Path::new(&format!("/f{i}.png")), 1, 1, i as u64);
        }
        assert_eq!(cat.recent().len(), MAX_RECENT);
        // The most recent insert is at the front.
        assert_eq!(
            cat.recent()[0].path.as_path(),
            Path::new(&format!("/f{}.png", MAX_RECENT + 7))
        );
    }
}
