//! What a camera RAW file decodes to, as iAi takes it from the library that
//! decodes it (`crates/iai-raw`), and, in [`ffi`], the C interface of that
//! library when it is a file of its own (`iai_raw.dll`) beside the app rather
//! than linked into it.
//!
//! This crate is the app's side of that boundary. It holds no decoder code and
//! has no dependencies.

use std::ops::Deref;

pub mod ffi;

/// Which of the library's two decoders read the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoder {
    Rawloader,
    Rawler,
}

/// How the picture is to be turned to stand upright, named as EXIF numbers it
/// (tag 0x0112).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Orientation {
    Normal,
    HorizontalFlip,
    Rotate180,
    VerticalFlip,
    Transpose,
    Rotate90,
    Transverse,
    Rotate270,
    Unknown,
}

impl Orientation {
    /// From the EXIF number, 1 to 8; any other is [`Orientation::Unknown`].
    pub fn from_exif(number: u16) -> Self {
        match number {
            1 => Self::Normal,
            2 => Self::HorizontalFlip,
            3 => Self::Rotate180,
            4 => Self::VerticalFlip,
            5 => Self::Transpose,
            6 => Self::Rotate90,
            7 => Self::Transverse,
            8 => Self::Rotate270,
            _ => Self::Unknown,
        }
    }

    /// The EXIF number; 0 for [`Orientation::Unknown`].
    pub fn exif(self) -> u16 {
        match self {
            Self::Unknown => 0,
            Self::Normal => 1,
            Self::HorizontalFlip => 2,
            Self::Rotate180 => 3,
            Self::VerticalFlip => 4,
            Self::Transpose => 5,
            Self::Rotate90 => 6,
            Self::Transverse => 7,
            Self::Rotate270 => 8,
        }
    }
}

/// Side of the table [`Cfa::color_at`] reads: a multiple of every pattern
/// size a [`Cfa`] takes, so that a position wraps into it by a constant.
const TILE: usize = 24;

/// The colour filter array of a sensor: which of four colours (0 red, 1 green,
/// 2 blue, 3 a fourth) the photosite at a row and column sees. The pattern
/// repeats every `width` columns and `height` rows.
#[derive(Clone)]
pub struct Cfa {
    /// The pattern as the decoder names it, row after row ("RGGB").
    pub name: String,
    width: usize,
    height: usize,
    tile: [[u8; TILE]; TILE],
}

impl Cfa {
    /// No pattern: a monochrome sensor, or a picture already demosaiced.
    pub fn none() -> Self {
        Self {
            name: String::new(),
            width: 0,
            height: 0,
            tile: [[0; TILE]; TILE],
        }
    }

    /// The pattern `colors` (row after row, `width` to a row). Both sides
    /// have to divide 24; no side at all is [`Cfa::none`] under `name`.
    pub fn new(name: &str, width: usize, height: usize, colors: &[u8]) -> Result<Self, String> {
        let mut cfa = Self::none();
        cfa.name = name.to_owned();
        if width == 0 || height == 0 {
            return Ok(cfa);
        }
        if TILE % width != 0 || TILE % height != 0 {
            return Err(format!("CFA pattern of {width}x{height} is not supported"));
        }
        if colors.len() != width * height {
            return Err(format!(
                "CFA pattern of {width}x{height} comes with {} colours",
                colors.len()
            ));
        }
        if let Some(odd) = colors.iter().find(|color| **color > 3) {
            return Err(format!("CFA colour {odd} is not one of 0 to 3"));
        }
        cfa.width = width;
        cfa.height = height;
        for (row, line) in cfa.tile.iter_mut().enumerate() {
            for (col, cell) in line.iter_mut().enumerate() {
                *cell = colors[(row % height) * width + col % width];
            }
        }
        Ok(cfa)
    }

    /// Whether there is a pattern.
    pub fn is_valid(&self) -> bool {
        self.width != 0 && self.height != 0
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// The colour at a photosite; 0 where there is no pattern. Called from the
    /// innermost loops of the demosaic.
    #[inline]
    pub fn color_at(&self, row: usize, col: usize) -> usize {
        self.tile[row % TILE][col % TILE] as usize
    }

    /// One repeat of the pattern, row after row: what [`Cfa::new`] takes.
    pub fn colors(&self) -> Vec<u8> {
        let mut colors = Vec::with_capacity(self.width * self.height);
        for row in 0..self.height {
            colors.extend_from_slice(&self.tile[row][..self.width]);
        }
        colors
    }
}

impl std::fmt::Debug for Cfa {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Cfa({:?})", self.name)
    }
}

/// The samples of a decoded picture: a `Vec` of the app's own, or memory the
/// library lends the app until this is dropped. Read as a slice either way.
pub struct Samples<T>(Store<T>);

enum Store<T> {
    Owned(Vec<T>),
    Lent {
        ptr: *const T,
        len: usize,
        _lender: ffi::Lender,
    },
}

// What is lent is never written to once the library has handed it over, and
// the library takes it back from any thread (see `ffi::Lender`).
unsafe impl<T: Send + Sync> Send for Samples<T> {}
unsafe impl<T: Send + Sync> Sync for Samples<T> {}

impl<T> Deref for Samples<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        match &self.0 {
            Store::Owned(samples) => samples,
            // `ffi::receive` checked the pointer and the length it lends.
            Store::Lent { ptr, len, .. } => unsafe { std::slice::from_raw_parts(*ptr, *len) },
        }
    }
}

impl<T> Samples<T> {
    /// # Safety
    /// `len` samples lie at `ptr`, aligned, and stay there unchanged until
    /// `lender` is dropped.
    pub(crate) unsafe fn lent(ptr: *const T, len: usize, lender: ffi::Lender) -> Self {
        Self(Store::Lent {
            ptr,
            len,
            _lender: lender,
        })
    }
}

impl<T> From<Vec<T>> for Samples<T> {
    fn from(samples: Vec<T>) -> Self {
        Self(Store::Owned(samples))
    }
}

/// The picture as the sensor recorded it: `width * height * cpp` samples.
pub enum RawImageData {
    /// What almost every format holds.
    Integer(Samples<u16>),
    /// Some DNGs.
    Float(Samples<f32>),
}

/// A decoded RAW file: the sensor's samples and what is needed to render them.
/// Arrays of four are in the order red, green, blue, fourth colour.
pub struct RawImage {
    pub decoder: Decoder,
    /// Camera make and model as the file has them.
    pub make: String,
    pub model: String,
    /// The same, tidied to one spelling per camera.
    pub clean_make: String,
    pub clean_model: String,
    /// The whole sensor, masked borders included.
    pub width: usize,
    pub height: usize,
    /// Samples per pixel: 1 for a mosaic or a monochrome sensor, 3 for a
    /// picture already demosaiced.
    pub cpp: usize,
    /// White balance as shot; zeros where the file has none.
    pub wb_coeffs: [f32; 4],
    /// The white balance to fall back on then: a neutral at 6500 K derived
    /// from `xyz_to_cam`, green being 1.
    pub neutral_wb: [f32; 4],
    pub whitelevels: [u16; 4],
    pub blacklevels: [u16; 4],
    /// From CIE XYZ to the camera's colours.
    pub xyz_to_cam: [[f32; 3]; 4],
    /// The way back, from `xyz_to_cam` with each row first scaled to sum to
    /// one: the camera's colours to XYZ.
    pub cam_to_xyz_normalized: [[f32; 4]; 3],
    pub cfa: Cfa,
    /// What to cut off each side to be left with the picture: top, right,
    /// bottom, left.
    pub crops: [usize; 4],
    /// The sensor's masked rectangles: top, right, bottom, left.
    pub blackareas: Vec<[u64; 4]>,
    pub orientation: Orientation,
    pub data: RawImageData,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_repeats_over_the_sensor_and_none_reads_as_zero() {
        let bayer = Cfa::new("RGGB", 2, 2, &[0, 1, 1, 2]).unwrap();
        assert!(bayer.is_valid());
        assert_eq!(
            [
                bayer.color_at(0, 0),
                bayer.color_at(0, 1),
                bayer.color_at(1, 0),
                bayer.color_at(1, 1)
            ],
            [0, 1, 1, 2]
        );
        assert_eq!(bayer.color_at(4001, 6000), 1);
        assert_eq!(bayer.color_at(4001, 6001), 2);
        assert_eq!(bayer.colors(), [0, 1, 1, 2]);

        let none = Cfa::new("", 0, 0, &[]).unwrap();
        assert!(!none.is_valid());
        assert_eq!(none.color_at(7, 9), 0);
        assert!(none.colors().is_empty());
    }

    #[test]
    fn every_pattern_size_the_decoders_know_wraps_exactly() {
        for (width, height) in [(2, 2), (6, 6), (2, 8), (12, 12)] {
            let colors: Vec<u8> = (0..width * height).map(|i| (i % 4) as u8).collect();
            let cfa = Cfa::new("", width, height, &colors).unwrap();
            for row in 0..100 {
                for col in 0..100 {
                    assert_eq!(
                        cfa.color_at(row, col),
                        colors[(row % height) * width + col % width] as usize,
                        "{width}x{height} at {row},{col}"
                    );
                }
            }
            assert_eq!(cfa.colors(), colors);
        }
    }

    #[test]
    fn a_pattern_the_table_cannot_hold_is_refused() {
        assert!(Cfa::new("", 5, 2, &[0; 10]).is_err());
        assert!(Cfa::new("", 2, 2, &[0, 1, 1]).is_err());
        assert!(Cfa::new("", 2, 2, &[0, 1, 1, 4]).is_err());
    }

    #[test]
    fn an_orientation_keeps_its_exif_number() {
        for number in 0..=9u16 {
            let orientation = Orientation::from_exif(number);
            let expected = if (1..=8).contains(&number) { number } else { 0 };
            assert_eq!(orientation.exif(), expected);
        }
        assert_eq!(Orientation::from_exif(6), Orientation::Rotate90);
    }
}
