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
    /// Returns `true` when this call newly enqueued work — the caller uses that
    /// to kick a redraw so the first poll after enqueuing sees the outstanding
    /// request and keeps the frame loop alive until the thumbnail lands.
    pub fn request(&mut self, path: &Path) -> bool {
        if self.tex.contains_key(path) || self.requested.contains(path) {
            return false;
        }
        self.ensure_worker();
        self.requested.insert(path.to_path_buf());
        if let Some(tx) = &self.req_tx {
            let _ = tx.send(path.to_path_buf());
        }
        true
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
        // Keep the frame loop alive while generations are outstanding so streamed
        // thumbnails appear on their own; the loop goes quiet once all land.
        if !self.requested.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
    }

    pub fn get(&self, path: &Path) -> Option<&egui::TextureHandle> {
        self.tex.get(path)
    }
}

/// The folder-browser grid state (Track B, B3): the chosen folder, the images
/// scanned from it, and the multi-selection.
#[derive(Default)]
pub struct LibraryGrid {
    pub folder: Option<PathBuf>,
    /// Supported image files in `folder`, sorted by name.
    pub entries: Vec<PathBuf>,
    /// Selected entries (subset of `entries`), for "Open Selected".
    pub selected: HashSet<PathBuf>,
    /// The last plain/Ctrl-clicked entry — the fixed end of a Shift range-select.
    pub anchor: Option<PathBuf>,
}

impl LibraryGrid {
    /// Scan `dir` for supported images and reset the selection. Cheap: a single
    /// `read_dir` with an extension filter — no decoding (thumbnails are lazy).
    pub fn scan(&mut self, dir: PathBuf) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_supported_image(p))
            .collect();
        entries.sort_by(|a, b| {
            a.file_name()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .cmp(&b.file_name().unwrap_or_default().to_ascii_lowercase())
        });
        self.entries = entries;
        self.selected.clear();
        self.anchor = None;
        self.folder = Some(dir);
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|p| p == path)
    }

    /// Plain click: select only `path`, and make it the range anchor.
    pub fn select_one(&mut self, path: PathBuf) {
        self.selected.clear();
        self.selected.insert(path.clone());
        self.anchor = Some(path);
    }

    /// Ctrl/Cmd click: add/remove `path` from the selection, re-anchoring here.
    pub fn toggle(&mut self, path: PathBuf) {
        if !self.selected.remove(&path) {
            self.selected.insert(path.clone());
        }
        self.anchor = Some(path);
    }

    /// Shift click: replace the selection with the contiguous range between the
    /// anchor and `path` (inclusive). With no anchor yet, this anchors on `path`.
    pub fn select_range_to(&mut self, path: &Path) {
        if self.anchor.is_none() {
            self.anchor = Some(path.to_path_buf());
        }
        let Some(to) = self.index_of(path) else {
            return;
        };
        let from = self
            .anchor
            .as_deref()
            .and_then(|a| self.index_of(a))
            .unwrap_or(to);
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        self.selected = self.entries[lo..=hi].iter().cloned().collect();
        // The anchor stays put so the range can be re-dragged from the same start.
    }

    /// Ctrl+A: select every entry (anchor moves to the first).
    pub fn select_all(&mut self) {
        self.selected = self.entries.iter().cloned().collect();
        self.anchor = self.entries.first().cloned();
    }
}

/// True for files the grid shows: camera RAW (rendered from its embedded
/// preview) and the ordinary raster formats the thumbnailer can decode. PSD/PDF
/// are excluded — the thumbnail path (`image::open`) cannot render them.
fn is_supported_image(path: &Path) -> bool {
    if crate::formats::raw::is_raw_path(path) {
        return true;
    }
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "jfif" | "jpe" | "bmp" | "gif" | "tiff" | "tif" | "webp")
    )
}

/// The library subsystem held by [`App`]: the recent-files catalog, the shared
/// thumbnail cache, and the folder-browser grid.
pub struct LibraryShell {
    pub catalog: Catalog,
    pub thumbs: ThumbCache,
    pub grid: LibraryGrid,
}

impl LibraryShell {
    pub fn new() -> Self {
        Self {
            catalog: Catalog::load(),
            thumbs: ThumbCache::default(),
            grid: LibraryGrid::default(),
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
        let mut enqueued = false;
        let view: Vec<crate::ui::RecentThumb> = entries
            .iter()
            .map(|e| {
                enqueued |= self.lib.thumbs.request(&e.path);
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
            .collect();
        if enqueued {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        view
    }

    /// Build the Library grid view model: the chosen folder's images with any
    /// ready thumbnails attached. Thumbnails are requested lazily — only for the
    /// cards the UI reported visible last frame (see `handle_library_actions`) —
    /// so a large folder never floods the generator. Called from `collect_ui_data`.
    pub(in crate::app) fn library_grid_view(&mut self) -> crate::ui::LibraryViewModel {
        let ctx = self.win.egui_ctx.clone();
        self.lib.thumbs.poll(&ctx);
        let grid = &self.lib.grid;
        let entries = grid
            .entries
            .iter()
            .map(|path| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let thumb = self.lib.thumbs.get(path).map(|t| (t.id(), t.size_vec2()));
                crate::ui::LibraryEntry {
                    path: path.clone(),
                    name,
                    selected: grid.selected.contains(path),
                    thumb,
                }
            })
            .collect();
        crate::ui::LibraryViewModel {
            folder: grid
                .folder
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            entries,
            selected_count: grid.selected.len(),
        }
    }

    /// Apply the Library grid intents: folder browse, selection edits, and
    /// opening files into the editor. Called each frame from `apply_ui_actions`.
    pub(in crate::app) fn handle_library_actions(&mut self, actions: &mut crate::ui::UiActions) {
        if let Some(v) = actions.chrome.show_library.take() {
            self.shell.ui.show_library = v;
            // Library and the welcome screen are mutually exclusive full-screen
            // surfaces; entering one leaves the other.
            if v {
                self.shell.ui.show_welcome = false;
            }
        }

        if actions.library.open_folder {
            self.pick_library_folder();
        }

        // Request thumbnails only for the cards the UI reported visible this
        // frame, so scrolling a big folder never thrashes the resident-texture
        // cache. The generator dedupes and disk-caches, so re-requests are cheap.
        let mut enqueued = false;
        for path in actions.library.visible_thumbs.drain(..) {
            enqueued |= self.lib.thumbs.request(&path);
        }
        // Kick one redraw so the next poll sees the fresh requests and then
        // sustains the loop itself until every thumbnail has landed.
        if enqueued {
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }

        if let Some((path, kind)) = actions.library.select_entry.take() {
            match kind {
                crate::ui::LibrarySelect::Replace => self.lib.grid.select_one(path),
                crate::ui::LibrarySelect::Toggle => self.lib.grid.toggle(path),
                crate::ui::LibrarySelect::Range => self.lib.grid.select_range_to(&path),
            }
        }
        if actions.library.clear_selection {
            self.lib.grid.selected.clear();
            self.lib.grid.anchor = None;
        }

        if let Some(path) = actions.library.open_entry.take() {
            self.open_library_paths(vec![path]);
        }
        if actions.library.open_selected {
            // Open in grid order (not the HashSet's arbitrary order).
            let paths: Vec<PathBuf> = self
                .lib
                .grid
                .entries
                .iter()
                .filter(|p| self.lib.grid.selected.contains(*p))
                .cloned()
                .collect();
            self.open_library_paths(paths);
        }
    }

    /// Open the given library files into the editor and leave the Library view.
    /// Missing paths are skipped; if none remain, the view is unchanged.
    fn open_library_paths(&mut self, paths: Vec<PathBuf>) {
        let existing: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();
        if existing.is_empty() {
            self.shell.status_msg = "File not found".to_string();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }
        self.shell.ui.show_library = false;
        self.start_load_paths(existing);
    }

    /// Open the OS folder picker for the Library grid (deferred to a worker
    /// thread; the result lands in `poll_file_dialog` and triggers a scan).
    pub(in crate::app) fn pick_library_folder(&mut self) {
        if self.jobs.pending_file_dialog.is_some() {
            return;
        }
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = crate::file_io::dialog_parent(window);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Some(dir) = crate::file_io::dialog_pick_folder(parent) {
                let _ = tx.send(crate::file_io::FileDialogResult::PickedFolder(dir));
            }
        });
        self.jobs.pending_file_dialog = Some(rx);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
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
    fn supported_image_covers_raw_and_common_rasters() {
        assert!(is_supported_image(Path::new("a.jpg")));
        assert!(is_supported_image(Path::new("A.JPG"))); // case-insensitive
        assert!(is_supported_image(Path::new("b.png")));
        assert!(is_supported_image(Path::new("c.tiff")));
        assert!(is_supported_image(Path::new("shot.CR2"))); // RAW
        assert!(is_supported_image(Path::new("shot.nef")));
        // Documents the thumbnailer cannot render, and non-images, are excluded.
        assert!(!is_supported_image(Path::new("doc.psd")));
        assert!(!is_supported_image(Path::new("doc.pdf")));
        assert!(!is_supported_image(Path::new("proj.iai")));
        assert!(!is_supported_image(Path::new("notes.txt")));
        assert!(!is_supported_image(Path::new("noext")));
    }

    #[test]
    fn grid_scan_filters_and_sorts() {
        let dir = std::env::temp_dir().join(format!("iai_lib_scan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b.png", "a.jpg", "c.PNG", "skip.txt", "notes.pdf"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let mut grid = LibraryGrid::default();
        grid.selected.insert(PathBuf::from("stale"));
        grid.scan(dir.clone());
        let names: Vec<String> = grid
            .entries
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.jpg", "b.png", "c.PNG"]);
        assert_eq!(grid.folder.as_deref(), Some(dir.as_path()));
        assert!(grid.selected.is_empty(), "scan resets the selection");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn grid_of(names: &[&str]) -> LibraryGrid {
        let mut g = LibraryGrid::default();
        g.entries = names.iter().map(PathBuf::from).collect();
        g
    }

    #[test]
    fn select_range_covers_anchor_to_target_either_direction() {
        let mut g = grid_of(&["a", "b", "c", "d", "e"]);
        g.select_one(PathBuf::from("b")); // anchor = b
        g.select_range_to(Path::new("d"));
        let mut got: Vec<_> = g.selected.iter().cloned().collect();
        got.sort();
        assert_eq!(
            got,
            vec![PathBuf::from("b"), PathBuf::from("c"), PathBuf::from("d")]
        );

        // A range upward from the same anchor replaces the selection, anchor kept.
        g.select_range_to(Path::new("a"));
        let mut got: Vec<_> = g.selected.iter().cloned().collect();
        got.sort();
        assert_eq!(got, vec![PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(g.anchor.as_deref(), Some(Path::new("b")));
    }

    #[test]
    fn toggle_adds_and_removes_and_reanchors() {
        let mut g = grid_of(&["a", "b", "c"]);
        g.select_one(PathBuf::from("a"));
        g.toggle(PathBuf::from("c")); // add
        assert_eq!(g.selected.len(), 2);
        assert_eq!(g.anchor.as_deref(), Some(Path::new("c")));
        g.toggle(PathBuf::from("c")); // remove
        assert!(!g.selected.contains(Path::new("c")));
        assert!(g.selected.contains(Path::new("a")));
    }

    #[test]
    fn select_all_selects_every_entry() {
        let mut g = grid_of(&["a", "b", "c"]);
        g.select_all();
        assert_eq!(g.selected.len(), 3);
        assert_eq!(g.anchor.as_deref(), Some(Path::new("a")));
    }

    #[test]
    fn shift_click_with_no_anchor_selects_just_that_entry() {
        let mut g = grid_of(&["a", "b", "c"]);
        g.select_range_to(Path::new("b"));
        assert_eq!(
            g.selected.iter().cloned().collect::<Vec<_>>(),
            vec![PathBuf::from("b")]
        );
        assert_eq!(g.anchor.as_deref(), Some(Path::new("b")));
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
