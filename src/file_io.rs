use crate::core::canvas::{Canvas, MAX_DIMENSION};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum FileError {
    Io(String),
    UnsupportedFormat(String),
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            FileError::Io(s) => write!(f, "{}", s),
            FileError::UnsupportedFormat(s) => write!(f, "Unsupported format: {}", s),
        }
    }
}

pub fn save(canvas: &Canvas, path: &Path) -> Result<PathBuf, FileError> {
    let path = if path.extension().is_none() {
        path.with_extension("png")
    } else {
        path.to_path_buf()
    };
    let pixels = canvas.export_flat();
    image::save_buffer(
        &path,
        &pixels,
        canvas.width,
        canvas.height,
        image::ColorType::Rgba8,
    )
    .map_err(|e| FileError::Io(e.to_string()))?;
    Ok(path)
}

pub fn load(path: &Path) -> Result<Canvas, FileError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "bmp" | "gif" | "tiff" | "webp" => {}
        other => return Err(FileError::UnsupportedFormat(other.to_string())),
    }

    let mut reader = image::ImageReader::open(path)
        .map_err(|e| FileError::Io(e.to_string()))?
        .with_guessed_format()
        .map_err(|e| FileError::Io(e.to_string()))?;

    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    reader.limits(limits);

    let img = reader.decode().map_err(|e| {
        FileError::Io(format!(
            "{e} (max supported size is {MAX_DIMENSION}x{MAX_DIMENSION})"
        ))
    })?;

    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return Err(FileError::Io("image has a zero dimension".into()));
    }
    Ok(Canvas::from_rgba(rgba.into_raw(), w, h))
}

/// Result of a file-dialog session (sent from the worker thread to main via a channel).
pub enum FileDialogResult {
    /// Files chosen in the Open dialog (one or many; each → its own tab).
    OpenedMany(Vec<PathBuf>),
    SaveAs(PathBuf),
    Export(crate::formats::ExportFormat, PathBuf),
    /// A folder chosen in the Library grid browser (Track B) → scan for images.
    PickedFolder(PathBuf),
}

/// Send-safe parent window handle (just the HWND/HINSTANCE integers on Windows)
/// for moving to a worker thread — the raw `RawWindowHandle` holds pointers, so it's !Send.
/// Rebuilds the rwh handle on demand in `HasWindowHandle` for rfd's `set_parent`.
#[derive(Clone, Copy)]
pub struct DialogParent {
    hwnd: isize,
    hinstance: isize,
}

impl DialogParent {
    /// Native owner handle for Win32 dialogs that are not routed through rfd.
    pub fn hwnd(self) -> isize {
        self.hwnd
    }
}

impl winit::raw_window_handle::HasWindowHandle for DialogParent {
    fn window_handle(
        &self,
    ) -> Result<winit::raw_window_handle::WindowHandle<'_>, winit::raw_window_handle::HandleError>
    {
        use winit::raw_window_handle::{
            HandleError, RawWindowHandle, Win32WindowHandle, WindowHandle,
        };
        let hwnd = std::num::NonZeroIsize::new(self.hwnd).ok_or(HandleError::Unavailable)?;
        let mut h = Win32WindowHandle::new(hwnd);
        h.hinstance = std::num::NonZeroIsize::new(self.hinstance);
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(h)) })
    }
}

impl winit::raw_window_handle::HasDisplayHandle for DialogParent {
    fn display_handle(
        &self,
    ) -> Result<winit::raw_window_handle::DisplayHandle<'_>, winit::raw_window_handle::HandleError>
    {
        use winit::raw_window_handle::{DisplayHandle, RawDisplayHandle, WindowsDisplayHandle};
        Ok(unsafe {
            DisplayHandle::borrow_raw(RawDisplayHandle::Windows(WindowsDisplayHandle::new()))
        })
    }
}

/// Extract a winit window's HWND into a Send-safe `DialogParent` (Windows). Returns None
/// on other platforms → the dialog opens parentless (still works).
pub fn dialog_parent(window: &winit::window::Window) -> Option<DialogParent> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(DialogParent {
            hwnd: h.hwnd.get(),
            hinstance: h.hinstance.map(|x| x.get()).unwrap_or(0),
        }),
        _ => None,
    }
}

/// Open dialog allowing MULTIPLE files. CALL ON A WORKER THREAD.
pub fn dialog_open_many(parent: Option<DialogParent>) -> Option<Vec<PathBuf>> {
    let img_exts = &[
        "png", "jpg", "jpeg", "jfif", "jpe", "bmp", "gif", "tiff", "tif", "webp",
    ];
    // Camera RAW formats handled by the RAW importer (see formats/raw.rs).
    let raw_exts = &[
        "cr2", "crw", "nef", "nrw", "arw", "sr2", "srf", "raf", "orf", "rw2", "pef", "srw", "dng",
        "dcr", "dcs", "kdc", "mrw", "erf", "mef", "mos", "iiq", "3fr", "ari", "x3f",
    ];
    let mut dialog = rfd::FileDialog::new()
        .add_filter(
            "All supported",
            &[
                "iai", "png", "jpg", "jpeg", "jfif", "jpe", "bmp", "gif", "tiff", "tif", "webp",
                "psd", "psb", "pdf", "cr2", "crw", "nef", "nrw", "arw", "sr2", "srf", "raf", "orf",
                "rw2", "pef", "srw", "dng", "dcr", "dcs", "kdc", "mrw", "erf", "mef", "mos", "iiq",
                "3fr", "ari", "x3f",
            ],
        )
        .add_filter("iAi Project", &["iai"])
        .add_filter("Images", img_exts)
        .add_filter("Photoshop (PSD)", &["psd", "psb"])
        .add_filter("PDF Document", &["pdf"])
        .add_filter("RAW Photo", raw_exts)
        .add_filter("All files", &["*"])
        .set_title("Open Files");
    if let Some(p) = parent {
        dialog = dialog.set_parent(&p);
    }
    dialog.pick_files()
}

/// Pick a folder for the Library grid browser. CALL ON A WORKER THREAD.
pub fn dialog_pick_folder(parent: Option<DialogParent>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Choose Folder");
    if let Some(p) = parent {
        dialog = dialog.set_parent(&p);
    }
    dialog.pick_folder()
}

/// Open the Save As dialog. CALL ON A WORKER THREAD.
pub fn dialog_save(current: Option<PathBuf>, parent: Option<DialogParent>) -> Option<PathBuf> {
    let default = current
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("untitled.iai")
        .to_string();
    let mut dialog = rfd::FileDialog::new()
        .add_filter("iAi Project", &["iai"])
        .add_filter("PNG", &["png"])
        .add_filter("JPEG", &["jpg", "jpeg"])
        .set_file_name(default)
        .set_title("Save File");
    if let Some(p) = parent {
        dialog = dialog.set_parent(&p);
    }
    dialog.save_file()
}
