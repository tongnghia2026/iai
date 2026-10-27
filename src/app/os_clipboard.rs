//! The system clipboard's images (`iai_platform::clipboard`), within the
//! largest canvas iAi opens.

pub use iai_platform::clipboard::{OsClipboardImage, image_hash, read_image_ppi, write_image};

/// Read an image from the OS clipboard (None when it holds no image).
pub fn read_image() -> Result<Option<OsClipboardImage>, String> {
    iai_platform::clipboard::read_image(crate::core::canvas::MAX_DIMENSION)
}
