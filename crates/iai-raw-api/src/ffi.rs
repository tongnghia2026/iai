//! The C interface of the RAW library when it is a file of its own
//! (`iai_raw.dll` beside the app; `libiai_raw.so`, `libiai_raw.dylib`).
//!
//! The library exports four functions, all callable from any thread:
//!
//! ```c
//! // The number of this interface: ABI below. The app takes no other.
//! uint32_t           iai_raw_abi(void);
//! // Decode the file at `path` (UTF-8, `path_len` bytes, no terminator).
//! // Never null. The frame stays as it is until it is given to iai_raw_free.
//! const IaiRawFrame *iai_raw_decode_file(const uint8_t *path, size_t path_len);
//! // Give a frame back, once.
//! void               iai_raw_free(const IaiRawFrame *frame);
//! // The EXIF orientation (1 to 8) of the RAW file whose bytes these are,
//! // read without decoding the picture; 0 when it cannot be told.
//! uint32_t           iai_raw_orientation(const uint8_t *file, size_t file_len);
//! ```
//!
//! `IaiRawFrame` is [`Frame`]; its fields mean what the fields of
//! [`RawImage`](crate::RawImage) of the same names do. A library with these
//! four functions, whatever it is built from, is one the app can use.
// Memory handed across a C interface: unsafe by nature.
#![allow(unsafe_code)]

use std::ffi::c_void;

use crate::{Cfa, Decoder, Orientation, RawImage, RawImageData, Samples};

/// The number of the interface described here. It changes whenever [`Frame`]
/// or the meaning of a function does.
pub const ABI: u32 = 1;

/// The library's name, without what the system puts around it.
pub const LIBRARY: &str = "iai_raw";

pub const ABI_SYMBOL: &[u8] = b"iai_raw_abi\0";
pub const DECODE_FILE_SYMBOL: &[u8] = b"iai_raw_decode_file\0";
pub const FREE_SYMBOL: &[u8] = b"iai_raw_free\0";
pub const ORIENTATION_SYMBOL: &[u8] = b"iai_raw_orientation\0";

pub type AbiFn = unsafe extern "C" fn() -> u32;
pub type DecodeFileFn = unsafe extern "C" fn(path: *const u8, path_len: usize) -> *const Frame;
pub type FreeFn = unsafe extern "C" fn(frame: *const Frame);
pub type OrientationFn = unsafe extern "C" fn(file: *const u8, file_len: usize) -> u32;

/// `len` bytes at `ptr`; text is UTF-8 without a terminator.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Span {
    pub ptr: *const u8,
    pub len: usize,
}

impl Span {
    const EMPTY: Self = Self {
        ptr: std::ptr::null(),
        len: 0,
    };

    fn of(bytes: &[u8]) -> Self {
        Self {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
        }
    }

    /// # Safety
    /// The span is one a library set: `len` readable bytes at `ptr`.
    unsafe fn bytes<'a>(self) -> &'a [u8] {
        if self.ptr.is_null() || self.len == 0 {
            &[]
        } else {
            // SAFETY: the caller vouches for `len` readable bytes at `ptr`.
            unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
        }
    }

    /// # Safety
    /// As [`Span::bytes`].
    unsafe fn text(self) -> String {
        // SAFETY: the caller vouches for the span, as `bytes` asks.
        String::from_utf8_lossy(unsafe { self.bytes() }).into_owned()
    }
}

/// `sample_kind` of a [`Frame`] whose samples are `uint16_t`.
pub const SAMPLES_U16: u32 = 0;
/// `sample_kind` of a [`Frame`] whose samples are `float`.
pub const SAMPLES_F32: u32 = 1;

/// What a decode answers with.
#[repr(C)]
pub struct Frame {
    /// Why the file could not be decoded; empty when it was, and only then
    /// does the rest say anything.
    pub error: Span,
    /// 0 rawloader, 1 rawler.
    pub decoder: u32,
    /// The EXIF number, 0 for unknown.
    pub orientation: u32,
    pub width: u64,
    pub height: u64,
    pub cpp: u64,
    pub make: Span,
    pub model: Span,
    pub clean_make: Span,
    pub clean_model: Span,
    pub wb_coeffs: [f32; 4],
    pub neutral_wb: [f32; 4],
    pub whitelevels: [u16; 4],
    pub blacklevels: [u16; 4],
    pub xyz_to_cam: [[f32; 3]; 4],
    pub cam_to_xyz_normalized: [[f32; 4]; 3],
    pub cfa_name: Span,
    /// The size of the repeating pattern; 0 by 0 for none.
    pub cfa_width: u32,
    pub cfa_height: u32,
    /// `cfa_width * cfa_height` colours (0 to 3), row after row.
    pub cfa_colors: Span,
    pub crops: [u64; 4],
    pub blackareas: *const [u64; 4],
    pub blackareas_len: usize,
    /// [`SAMPLES_U16`] or [`SAMPLES_F32`].
    pub sample_kind: u32,
    /// `samples_len` (`width * height * cpp`) samples of that kind.
    pub samples: *const c_void,
    pub samples_len: usize,
}

impl Frame {
    fn empty() -> Self {
        Self {
            error: Span::EMPTY,
            decoder: 0,
            orientation: 0,
            width: 0,
            height: 0,
            cpp: 0,
            make: Span::EMPTY,
            model: Span::EMPTY,
            clean_make: Span::EMPTY,
            clean_model: Span::EMPTY,
            wb_coeffs: [0.0; 4],
            neutral_wb: [0.0; 4],
            whitelevels: [0; 4],
            blacklevels: [0; 4],
            xyz_to_cam: [[0.0; 3]; 4],
            cam_to_xyz_normalized: [[0.0; 4]; 3],
            cfa_name: Span::EMPTY,
            cfa_width: 0,
            cfa_height: 0,
            cfa_colors: Span::EMPTY,
            crops: [0; 4],
            blackareas: std::ptr::null(),
            blackareas_len: 0,
            sample_kind: SAMPLES_U16,
            samples: std::ptr::null(),
            samples_len: 0,
        }
    }
}

/// The library's side: an answer, and what its frame points into.
#[repr(C)]
struct Held {
    frame: Frame,
    image: Option<RawImage>,
    error: String,
    cfa_colors: Vec<u8>,
}

impl Held {
    fn describe(&self) -> Frame {
        let mut frame = Frame::empty();
        frame.error = Span::of(self.error.as_bytes());
        let Some(image) = &self.image else {
            return frame;
        };
        frame.decoder = match image.decoder {
            Decoder::Rawloader => 0,
            Decoder::Rawler => 1,
        };
        frame.orientation = u32::from(image.orientation.exif());
        frame.width = image.width as u64;
        frame.height = image.height as u64;
        frame.cpp = image.cpp as u64;
        frame.make = Span::of(image.make.as_bytes());
        frame.model = Span::of(image.model.as_bytes());
        frame.clean_make = Span::of(image.clean_make.as_bytes());
        frame.clean_model = Span::of(image.clean_model.as_bytes());
        frame.wb_coeffs = image.wb_coeffs;
        frame.neutral_wb = image.neutral_wb;
        frame.whitelevels = image.whitelevels;
        frame.blacklevels = image.blacklevels;
        frame.xyz_to_cam = image.xyz_to_cam;
        frame.cam_to_xyz_normalized = image.cam_to_xyz_normalized;
        frame.cfa_name = Span::of(image.cfa.name.as_bytes());
        frame.cfa_width = image.cfa.width() as u32;
        frame.cfa_height = image.cfa.height() as u32;
        frame.cfa_colors = Span::of(&self.cfa_colors);
        frame.crops = image.crops.map(|side| side as u64);
        frame.blackareas = image.blackareas.as_ptr();
        frame.blackareas_len = image.blackareas.len();
        match &image.data {
            RawImageData::Integer(samples) => {
                frame.sample_kind = SAMPLES_U16;
                frame.samples = samples.as_ptr().cast();
                frame.samples_len = samples.len();
            }
            RawImageData::Float(samples) => {
                frame.sample_kind = SAMPLES_F32;
                frame.samples = samples.as_ptr().cast();
                frame.samples_len = samples.len();
            }
        }
        frame
    }
}

/// The library's answer to a decode: a frame for the app to read, kept until
/// the app gives it to [`take_back`].
pub fn lend(decoded: Result<RawImage, String>) -> *const Frame {
    let (image, error) = match decoded {
        Ok(image) => (Some(image), String::new()),
        Err(why) if why.is_empty() => (None, "the file could not be decoded".to_owned()),
        Err(why) => (None, why),
    };
    let cfa_colors = image
        .as_ref()
        .map(|image| image.cfa.colors())
        .unwrap_or_default();
    // Boxed first: the frame points into what the box holds.
    let mut held = Box::new(Held {
        frame: Frame::empty(),
        image,
        error,
        cfa_colors,
    });
    held.frame = held.describe();
    Box::into_raw(held) as *const Frame
}

/// Drop what a frame of [`lend`] kept.
///
/// # Safety
/// `frame` is a pointer [`lend`] returned (or null), given back once.
pub unsafe fn take_back(frame: *const Frame) {
    if !frame.is_null() {
        // SAFETY: `lend` made the pointer with `Box::into_raw`, and it comes
        // back once.
        drop(unsafe { Box::from_raw(frame as *mut Held) });
    }
}

/// A frame the library is to have back: given to its free function on drop.
pub(crate) struct Lender {
    frame: *const Frame,
    free: FreeFn,
}

// SAFETY: the library frees a frame from whichever thread hands it over.
unsafe impl Send for Lender {}
// SAFETY: a lender is only ever dropped, never read through.
unsafe impl Sync for Lender {}

impl Drop for Lender {
    fn drop(&mut self) {
        // SAFETY: the frame came from this library's decode and goes back to
        // its free function once, here.
        unsafe { (self.free)(self.frame) }
    }
}

/// # Safety
/// `ptr` and `len` are the samples of a frame, of `kind` as read from it.
unsafe fn samples<T>(ptr: *const c_void, len: usize, lender: Lender) -> Result<Samples<T>, String> {
    if len == 0 {
        return Ok(Vec::new().into());
    }
    let ptr = ptr.cast::<T>();
    if ptr.is_null() || !ptr.is_aligned() {
        return Err("the RAW library handed over no usable samples".to_owned());
    }
    // SAFETY: the caller vouches that these are the frame's samples, which
    // stay until the lender is dropped; null and alignment are checked above.
    Ok(unsafe { Samples::lent(ptr, len, lender) })
}

/// The app's side: the picture a frame describes. The samples stay the
/// library's memory, given back when the picture is dropped; a frame that
/// holds an error, or one that does not add up, is given back at once.
///
/// # Safety
/// `frame` is what a library's `iai_raw_decode_file` returned and `free` is
/// the same library's `iai_raw_free`. The caller does not touch `frame` again,
/// and the library stays loaded for as long as the picture lives.
pub unsafe fn receive(frame: *const Frame, free: FreeFn) -> Result<RawImage, String> {
    if frame.is_null() {
        return Err("the RAW library answered with nothing".to_owned());
    }
    let lender = Lender { frame, free };
    // SAFETY: not null, and the caller vouches it is a frame the library
    // returned and has not taken back.
    let frame = unsafe { &*frame };
    // SAFETY: every span read below is one of `frame`'s, which the library set.
    let text = |span: Span| unsafe { span.text() };
    if frame.error.len != 0 {
        return Err(text(frame.error));
    }
    let size = |value: u64| {
        usize::try_from(value).map_err(|_| "the RAW library's picture is too large".to_owned())
    };
    let (width, height, cpp) = (size(frame.width)?, size(frame.height)?, size(frame.cpp)?);
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(cpp));
    if expected != Some(frame.samples_len) {
        return Err(format!(
            "the RAW library's picture does not add up: {width}x{height}x{cpp} with {} samples",
            frame.samples_len
        ));
    }
    let decoder = match frame.decoder {
        0 => Decoder::Rawloader,
        1 => Decoder::Rawler,
        other => {
            return Err(format!(
                "the RAW library names an unknown decoder ({other})"
            ))
        }
    };
    let cfa = Cfa::new(
        &text(frame.cfa_name),
        frame.cfa_width as usize,
        frame.cfa_height as usize,
        // SAFETY: a span of `frame`'s, as above.
        unsafe { frame.cfa_colors.bytes() },
    )?;
    let blackareas = if frame.blackareas.is_null() || frame.blackareas_len == 0 {
        Vec::new()
    } else {
        // SAFETY: the library set `blackareas_len` areas at `blackareas`.
        unsafe { std::slice::from_raw_parts(frame.blackareas, frame.blackareas_len) }.to_vec()
    };
    let mut crops = [0usize; 4];
    for (side, value) in crops.iter_mut().zip(frame.crops) {
        *side = size(value)?;
    }
    let mut image = RawImage {
        decoder,
        make: text(frame.make),
        model: text(frame.model),
        clean_make: text(frame.clean_make),
        clean_model: text(frame.clean_model),
        width,
        height,
        cpp,
        wb_coeffs: frame.wb_coeffs,
        neutral_wb: frame.neutral_wb,
        whitelevels: frame.whitelevels,
        blacklevels: frame.blacklevels,
        xyz_to_cam: frame.xyz_to_cam,
        cam_to_xyz_normalized: frame.cam_to_xyz_normalized,
        cfa,
        crops,
        blackareas,
        orientation: Orientation::from_exif(u16::try_from(frame.orientation).unwrap_or(0)),
        data: RawImageData::Integer(Vec::new().into()),
    };
    image.data = match frame.sample_kind {
        SAMPLES_U16 => {
            // SAFETY: the samples pointer, length and kind are the frame's.
            let samples = unsafe { samples(frame.samples, frame.samples_len, lender) };
            RawImageData::Integer(samples?)
        }
        SAMPLES_F32 => {
            // SAFETY: as above.
            let samples = unsafe { samples(frame.samples, frame.samples_len, lender) };
            RawImageData::Float(samples?)
        }
        other => {
            return Err(format!(
                "the RAW library names an unknown kind of sample ({other})"
            ))
        }
    };
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn picture(data: RawImageData) -> RawImage {
        RawImage {
            decoder: Decoder::Rawler,
            make: "Canon".to_owned(),
            model: "Canon EOS R6m2".to_owned(),
            clean_make: "Canon".to_owned(),
            clean_model: "EOS R6 Mark II".to_owned(),
            width: 4,
            height: 2,
            cpp: 1,
            wb_coeffs: [2.1, 1.0, 1.6, f32::NAN],
            neutral_wb: [1.9, 1.0, 1.4, 0.0],
            whitelevels: [16383, 16383, 16383, 16383],
            blacklevels: [512, 513, 514, 515],
            xyz_to_cam: [
                [0.6, -0.2, -0.1],
                [-0.4, 1.2, 0.2],
                [-0.1, 0.2, 0.6],
                [0.0; 3],
            ],
            cam_to_xyz_normalized: [
                [1.5, 0.3, 0.2, 0.0],
                [0.4, 1.1, -0.1, 0.0],
                [0.1, -0.3, 1.7, 0.0],
            ],
            cfa: Cfa::new("GBRG", 2, 2, &[1, 2, 0, 1]).unwrap(),
            crops: [1, 2, 3, 4],
            blackareas: vec![[0, 4, 1, 0], [0, 2, 2, 0]],
            orientation: Orientation::Rotate270,
            data,
        }
    }

    /// Hand `decoded` over the interface as the library and the app do,
    /// counting in `freed` how often the frame is given back.
    fn across(
        decoded: Result<RawImage, String>,
        freed: &'static AtomicUsize,
    ) -> Result<RawImage, String> {
        // One counter per test: tests run side by side.
        static COUNTERS: std::sync::Mutex<Vec<(usize, &'static AtomicUsize)>> =
            std::sync::Mutex::new(Vec::new());
        unsafe extern "C" fn free(frame: *const Frame) {
            // A frame given back leaves the list: the allocator may hand its
            // address to the next test's frame.
            let mut counters = COUNTERS.lock().unwrap();
            let at = counters
                .iter()
                .position(|(address, _)| *address == frame as usize)
                .expect("a frame that was lent");
            let (_, freed) = counters.swap_remove(at);
            freed.fetch_add(1, Ordering::SeqCst);
            unsafe { take_back(frame) }
        }
        let frame = lend(decoded);
        COUNTERS.lock().unwrap().push((frame as usize, freed));
        unsafe { receive(frame, free) }
    }

    #[test]
    fn a_picture_crosses_whole_and_its_samples_go_back_when_it_is_dropped() {
        static FREED: AtomicUsize = AtomicUsize::new(0);
        let sent: Vec<u16> = (0..8).map(|i| 600 + i * 1000).collect();
        let got = across(
            Ok(picture(RawImageData::Integer(sent.clone().into()))),
            &FREED,
        )
        .unwrap();
        let reference = picture(RawImageData::Integer(Vec::new().into()));

        assert_eq!(got.decoder, reference.decoder);
        assert_eq!(
            (&got.make, &got.model, &got.clean_make, &got.clean_model),
            (
                &reference.make,
                &reference.model,
                &reference.clean_make,
                &reference.clean_model
            )
        );
        assert_eq!((got.width, got.height, got.cpp), (4, 2, 1));
        // Bit for bit, the not-a-number included.
        assert_eq!(
            got.wb_coeffs.map(f32::to_bits),
            reference.wb_coeffs.map(f32::to_bits)
        );
        assert_eq!(got.neutral_wb, reference.neutral_wb);
        assert_eq!(got.whitelevels, reference.whitelevels);
        assert_eq!(got.blacklevels, reference.blacklevels);
        assert_eq!(got.xyz_to_cam, reference.xyz_to_cam);
        assert_eq!(got.cam_to_xyz_normalized, reference.cam_to_xyz_normalized);
        assert_eq!(got.cfa.name, "GBRG");
        assert_eq!(got.cfa.colors(), [1, 2, 0, 1]);
        assert_eq!(got.crops, reference.crops);
        assert_eq!(got.blackareas, reference.blackareas);
        assert_eq!(got.orientation, Orientation::Rotate270);
        let RawImageData::Integer(samples) = &got.data else {
            panic!("integer samples were sent");
        };
        assert_eq!(&samples[..], &sent[..]);

        assert_eq!(FREED.load(Ordering::SeqCst), 0, "still lent");
        drop(got);
        assert_eq!(FREED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn float_samples_cross_as_floats() {
        static FREED: AtomicUsize = AtomicUsize::new(0);
        let sent = vec![0.25f32; 4 * 2 * 3];
        let mut image = picture(RawImageData::Float(sent.clone().into()));
        image.cpp = 3;
        image.cfa = Cfa::none();
        let got = across(Ok(image), &FREED).unwrap();
        assert!(!got.cfa.is_valid());
        let RawImageData::Float(samples) = &got.data else {
            panic!("float samples were sent");
        };
        assert_eq!(&samples[..], &sent[..]);
        drop(got);
        assert_eq!(FREED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_error_crosses_as_its_text_and_the_frame_goes_back_at_once() {
        static FREED: AtomicUsize = AtomicUsize::new(0);
        let why = "rawloader: no decoder; rawler: không đọc được".to_owned();
        assert_eq!(across(Err(why.clone()), &FREED).err(), Some(why));
        assert_eq!(FREED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_picture_that_does_not_add_up_is_refused_and_given_back() {
        static FREED: AtomicUsize = AtomicUsize::new(0);
        let image = picture(RawImageData::Integer(vec![0u16; 7].into()));
        let refused = across(Ok(image), &FREED);
        assert!(refused.is_err_and(|why| why.contains("does not add up")));
        assert_eq!(FREED.load(Ordering::SeqCst), 1);
    }
}
