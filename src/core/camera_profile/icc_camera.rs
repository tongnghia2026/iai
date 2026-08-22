//! Strict loading and execution of scene-referred RGB camera ICC profiles.
//!
//! A generic ICC profile is not automatically a camera characterization. This
//! module accepts only input-class (`scnr`) RGB profiles whose PCS is XYZ or
//! Lab and which contain either a matrix/shaper input path or `AToB0`. It also
//! requires the ICC v4 `ciis` tag to identify scene or focal-plane
//! colorimetry. A caller may explicitly trust a profile selected by a camera
//! manifest when `ciis` is absent; an explicitly present non-scene value is
//! never overridden.
//!
//! ICC tone curves and CLUTs do not have uniform behavior outside their
//! encoded domain. In particular, Little CMS clips negative values through
//! ordinary non-linear gamma curves. Consequently the public conversion API
//! accepts finite input only in `[0, 1]` and never silently clamps signed or
//! HDR camera values. Finite out-of-gamut values produced by the transform are
//! retained. Callers that need an unbounded camera path must use a transform
//! whose math explicitly defines that domain (for example the DCP matrix path).
//!
//! This module does not discover profiles and deliberately does not consume
//! DNG `AsShotICCProfile`; selection and trust are resolver responsibilities.

use std::fmt;
use std::sync::Mutex;

use lcms2::{
    CIExyY, CIExyYTRIPLE, DisallowCache, Flags, InfoType, Intent, Locale, PixelFormat, Profile,
    TagSignature, ThreadContext, ToneCurve, Transform,
};

/// Hard allocation/input bound applied before passing bytes to Little CMS.
pub const MAX_SCENE_CAMERA_ICC_BYTES: usize = 16 * 1024 * 1024;

/// The normalized device-domain accepted by [`SceneCameraIcc::convert_rgb`].
pub const SCENE_CAMERA_ICC_INPUT_MIN: f32 = 0.0;
pub const SCENE_CAMERA_ICC_INPUT_MAX: f32 = 1.0;

const ICC_HEADER_BYTES: usize = 128;
const ICC_TAG_TABLE_HEADER_BYTES: usize = 4;
const ICC_TAG_ENTRY_BYTES: usize = 12;
const ICC_MIN_BYTES: usize = ICC_HEADER_BYTES + ICC_TAG_TABLE_HEADER_BYTES;

const PROFILE_CLASS_OFFSET: usize = 12;
const DATA_COLOR_SPACE_OFFSET: usize = 16;
const PCS_OFFSET: usize = 20;
const ICC_MAGIC_OFFSET: usize = 36;
const PROFILE_ID_OFFSET: usize = 84;

const INPUT_CLASS: [u8; 4] = *b"scnr";
const RGB_DATA: [u8; 4] = *b"RGB ";
const XYZ_DATA: [u8; 4] = *b"XYZ ";
const LAB_DATA: [u8; 4] = *b"Lab ";
const ICC_MAGIC: [u8; 4] = *b"acsp";
const SIGNATURE_TYPE: [u8; 4] = *b"sig ";
const CIIS_TAG: u32 = TagSignature::ColorimetricIntentImageStateTag as u32;
const SCOE_STATE: u32 = lcms2_sys::ColorimetricIntentImageState::SceneColorimetryEstimates as u32;
const FPCE_STATE: u32 =
    lcms2_sys::ColorimetricIntentImageState::FocalPlaneColorimetryEstimates as u32;

type FloatRgbTransform = Transform<[f32; 3], [f32; 3], ThreadContext, DisallowCache>;

/// Technical input path exposed by the accepted camera profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneCameraIccEncoding {
    /// Relative-colorimetric `DToB1` floating-point pipeline.
    DToB1,
    /// Relative-colorimetric `AToB1` pipeline.
    AToB1,
    /// Perceptual `AToB0` used by Little CMS as the relative-colorimetric
    /// fallback when `DToB1` and `AToB1` are absent.
    AToB0Fallback,
    /// RGB colorant matrix plus per-channel tone curves.
    MatrixShaper,
}

/// Accepted ICC profile-connection space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneCameraIccPcs {
    Xyz,
    Lab,
}

/// Evidence that the profile describes scene-referred camera values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneCameraIccImageState {
    /// ICC `ciis` is `scoe` (scene colorimetry estimates).
    SceneColorimetryEstimates,
    /// ICC `ciis` is `fpce` (focal-plane colorimetry estimates).
    FocalPlaneColorimetryEstimates,
    /// `ciis` was absent and the resolver explicitly trusted this candidate.
    TrustedSceneOverride,
}

/// Stable facts suitable for recording alongside resolver provenance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneCameraIccMetadata {
    pub description: Option<String>,
    pub byte_len: usize,
    pub encoded_icc_version: u32,
    pub profile_id: Option<[u8; 16]>,
    pub pcs: SceneCameraIccPcs,
    pub encoding: SceneCameraIccEncoding,
    pub image_state: SceneCameraIccImageState,
}

/// Why a candidate cannot be used as a scene-camera ICC transform.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneCameraIccError {
    TooSmall {
        actual: usize,
        minimum: usize,
    },
    TooLarge {
        actual: usize,
        maximum: usize,
    },
    DeclaredSizeMismatch {
        declared: usize,
        actual: usize,
    },
    InvalidIccMagic,
    UnsupportedDeviceClass {
        signature: [u8; 4],
    },
    UnsupportedColorSpace {
        signature: [u8; 4],
    },
    UnsupportedPcs {
        signature: [u8; 4],
    },
    MalformedTagTable,
    MissingImageState,
    MalformedImageState,
    UnsupportedImageState {
        signature: [u8; 4],
    },
    InvalidProfile,
    MissingInputTransform,
    UnusableFloatTransform,
    PixelCountMismatch {
        input: usize,
        output: usize,
    },
    TooManyPixels {
        actual: usize,
        maximum: usize,
    },
    NonFiniteInput {
        pixel: usize,
        channel: usize,
        value: f32,
    },
    InputOutOfRange {
        pixel: usize,
        channel: usize,
        value: f32,
    },
    NonFiniteOutput {
        pixel: usize,
        channel: usize,
        value: f32,
    },
}

impl fmt::Display for SceneCameraIccError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall { actual, minimum } => {
                write!(f, "camera ICC is {actual} bytes; minimum is {minimum}")
            }
            Self::TooLarge { actual, maximum } => {
                write!(f, "camera ICC is {actual} bytes; limit is {maximum}")
            }
            Self::DeclaredSizeMismatch { declared, actual } => write!(
                f,
                "camera ICC declares {declared} bytes but contains {actual}"
            ),
            Self::InvalidIccMagic => f.write_str("camera ICC has no acsp header signature"),
            Self::UnsupportedDeviceClass { signature } => write!(
                f,
                "camera ICC class {} is not input class scnr",
                FourCc(*signature)
            ),
            Self::UnsupportedColorSpace { signature } => {
                write!(f, "camera ICC data space {} is not RGB", FourCc(*signature))
            }
            Self::UnsupportedPcs { signature } => write!(
                f,
                "camera ICC PCS {} is neither XYZ nor Lab",
                FourCc(*signature)
            ),
            Self::MalformedTagTable => f.write_str("camera ICC tag table is malformed"),
            Self::MissingImageState => {
                f.write_str("camera ICC is missing the required ciis scene-image-state tag")
            }
            Self::MalformedImageState => {
                f.write_str("camera ICC ciis scene-image-state tag is malformed")
            }
            Self::UnsupportedImageState { signature } => write!(
                f,
                "camera ICC ciis value {} is not scoe or fpce",
                FourCc(*signature)
            ),
            Self::InvalidProfile => f.write_str("Little CMS rejected the camera ICC"),
            Self::MissingInputTransform => {
                f.write_str("camera ICC has neither a matrix/shaper path nor AToB0")
            }
            Self::UnusableFloatTransform => {
                f.write_str("camera ICC cannot create an RGB-float relative-colorimetric transform")
            }
            Self::PixelCountMismatch { input, output } => write!(
                f,
                "camera ICC conversion has {input} input pixels and {output} output pixels"
            ),
            Self::TooManyPixels { actual, maximum } => write!(
                f,
                "camera ICC conversion has {actual} pixels; one call supports at most {maximum}"
            ),
            Self::NonFiniteInput {
                pixel,
                channel,
                value,
            } => write!(
                f,
                "camera ICC input pixel {pixel} channel {channel} is non-finite ({value})"
            ),
            Self::InputOutOfRange {
                pixel,
                channel,
                value,
            } => write!(
                f,
                "camera ICC input pixel {pixel} channel {channel} is outside [0, 1] ({value})"
            ),
            Self::NonFiniteOutput {
                pixel,
                channel,
                value,
            } => write!(
                f,
                "camera ICC output pixel {pixel} channel {channel} is non-finite ({value})"
            ),
        }
    }
}

impl std::error::Error for SceneCameraIccError {}

/// Validated RGB-float camera ICC to linear ProPhoto converter.
///
/// The Little CMS transform is created with `NO_CACHE`, making its immutable
/// execution safe to share. The originating [`ThreadContext`] is retained
/// until after the transform is dropped; the `Mutex` only supplies the safe
/// ownership/thread-safety boundary and is never locked during conversion.
#[derive(Debug)]
pub struct SceneCameraIcc {
    // Fields drop in declaration order: the transform must precede its context.
    transform: FloatRgbTransform,
    metadata: SceneCameraIccMetadata,
    _context: Mutex<ThreadContext>,
}

impl SceneCameraIcc {
    /// Validate a selected candidate and construct its technical transform.
    ///
    /// `trusted_scene` permits only a missing `ciis` tag. A malformed tag or an
    /// explicit non-scene value remains an error.
    pub fn new(bytes: &[u8], trusted_scene: bool) -> Result<Self, SceneCameraIccError> {
        let header = validate_header(bytes)?;
        let image_state = parse_image_state(bytes, trusted_scene)?;

        let context = ThreadContext::new();
        let source = Profile::new_icc_context(&context, bytes)
            .map_err(|_| SceneCameraIccError::InvalidProfile)?;

        let encoding = select_relative_encoding(
            source.has_tag(TagSignature::DToB1Tag),
            source.has_tag(TagSignature::AToB1Tag),
            source.has_tag(TagSignature::AToB0Tag),
            source.is_matrix_shaper(),
        )
        .ok_or(SceneCameraIccError::MissingInputTransform)?;

        let destination = linear_prophoto_profile(&context)
            .map_err(|_| SceneCameraIccError::UnusableFloatTransform)?;
        let transform = FloatRgbTransform::new_flags_context(
            &context,
            &source,
            PixelFormat::RGB_FLT,
            &destination,
            PixelFormat::RGB_FLT,
            Intent::RelativeColorimetric,
            Flags::NO_CACHE,
        )
        .map_err(|_| SceneCameraIccError::UnusableFloatTransform)?;

        let description = source
            .info(InfoType::Description, Locale::none())
            .map(|value| value.trim().chars().take(256).collect::<String>())
            .filter(|value| !value.is_empty());

        Ok(Self {
            transform,
            metadata: SceneCameraIccMetadata {
                description,
                byte_len: bytes.len(),
                encoded_icc_version: header.encoded_icc_version,
                profile_id: header.profile_id,
                pcs: header.pcs,
                encoding,
                image_state,
            },
            _context: Mutex::new(context),
        })
    }

    pub fn metadata(&self) -> &SceneCameraIccMetadata {
        &self.metadata
    }

    /// Convert one normalized camera RGB sample to linear ProPhoto RGB.
    pub fn convert_rgb(&self, rgb: [f32; 3]) -> Result<[f32; 3], SceneCameraIccError> {
        let mut output = [[0.0; 3]];
        self.convert_pixels(std::slice::from_ref(&rgb), &mut output)?;
        Ok(output[0])
    }

    /// Convert normalized camera RGB samples to linear ProPhoto RGB.
    ///
    /// Input is validated in full before the destination is modified. Output
    /// is not gamut-clipped; finite negative or greater-than-one results are
    /// meaningful linear ProPhoto values and are retained.
    pub fn convert_pixels(
        &self,
        input: &[[f32; 3]],
        output: &mut [[f32; 3]],
    ) -> Result<(), SceneCameraIccError> {
        if input.len() != output.len() {
            return Err(SceneCameraIccError::PixelCountMismatch {
                input: input.len(),
                output: output.len(),
            });
        }
        if input.len() > u32::MAX as usize {
            return Err(SceneCameraIccError::TooManyPixels {
                actual: input.len(),
                maximum: u32::MAX as usize,
            });
        }

        for (pixel, rgb) in input.iter().enumerate() {
            for (channel, &value) in rgb.iter().enumerate() {
                if !value.is_finite() {
                    return Err(SceneCameraIccError::NonFiniteInput {
                        pixel,
                        channel,
                        value,
                    });
                }
                if !(SCENE_CAMERA_ICC_INPUT_MIN..=SCENE_CAMERA_ICC_INPUT_MAX).contains(&value) {
                    return Err(SceneCameraIccError::InputOutOfRange {
                        pixel,
                        channel,
                        value,
                    });
                }
            }
        }

        self.transform.transform_pixels(input, output);

        for (pixel, rgb) in output.iter().enumerate() {
            for (channel, &value) in rgb.iter().enumerate() {
                if !value.is_finite() {
                    return Err(SceneCameraIccError::NonFiniteOutput {
                        pixel,
                        channel,
                        value,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Mirror Little CMS' device-to-PCS path selection for relative-colorimetric
/// transforms while retaining this module's deliberately narrow eligibility
/// rule: a profile must expose the ICC input baseline (`AToB0`) or a complete
/// RGB matrix/shaper path. Optional relative/float LUTs may override that
/// baseline once the profile is eligible.
fn select_relative_encoding(
    has_dtob1: bool,
    has_atob1: bool,
    has_atob0: bool,
    has_matrix_shaper: bool,
) -> Option<SceneCameraIccEncoding> {
    if !has_atob0 && !has_matrix_shaper {
        return None;
    }

    Some(if has_dtob1 {
        SceneCameraIccEncoding::DToB1
    } else if has_atob1 {
        SceneCameraIccEncoding::AToB1
    } else if has_atob0 {
        SceneCameraIccEncoding::AToB0Fallback
    } else {
        SceneCameraIccEncoding::MatrixShaper
    })
}

/// Perform the full strict classification, including float-transform
/// construction, without retaining a converter.
pub fn classify_scene_camera_icc(
    bytes: &[u8],
    trusted_scene: bool,
) -> Result<SceneCameraIccMetadata, SceneCameraIccError> {
    SceneCameraIcc::new(bytes, trusted_scene).map(|profile| profile.metadata.clone())
}

#[derive(Clone, Copy)]
struct ValidatedHeader {
    encoded_icc_version: u32,
    profile_id: Option<[u8; 16]>,
    pcs: SceneCameraIccPcs,
}

fn validate_header(bytes: &[u8]) -> Result<ValidatedHeader, SceneCameraIccError> {
    if bytes.len() > MAX_SCENE_CAMERA_ICC_BYTES {
        return Err(SceneCameraIccError::TooLarge {
            actual: bytes.len(),
            maximum: MAX_SCENE_CAMERA_ICC_BYTES,
        });
    }
    if bytes.len() < ICC_MIN_BYTES {
        return Err(SceneCameraIccError::TooSmall {
            actual: bytes.len(),
            minimum: ICC_MIN_BYTES,
        });
    }

    let declared = read_be_u32(bytes, 0).ok_or(SceneCameraIccError::TooSmall {
        actual: bytes.len(),
        minimum: ICC_MIN_BYTES,
    })? as usize;
    if declared != bytes.len() {
        return Err(SceneCameraIccError::DeclaredSizeMismatch {
            declared,
            actual: bytes.len(),
        });
    }
    if four_cc(bytes, ICC_MAGIC_OFFSET) != Some(ICC_MAGIC) {
        return Err(SceneCameraIccError::InvalidIccMagic);
    }

    let device_class = four_cc(bytes, PROFILE_CLASS_OFFSET).expect("validated ICC header length");
    if device_class != INPUT_CLASS {
        return Err(SceneCameraIccError::UnsupportedDeviceClass {
            signature: device_class,
        });
    }

    let color_space = four_cc(bytes, DATA_COLOR_SPACE_OFFSET).expect("validated ICC header length");
    if color_space != RGB_DATA {
        return Err(SceneCameraIccError::UnsupportedColorSpace {
            signature: color_space,
        });
    }

    let pcs_signature = four_cc(bytes, PCS_OFFSET).expect("validated ICC header length");
    let pcs = match pcs_signature {
        XYZ_DATA => SceneCameraIccPcs::Xyz,
        LAB_DATA => SceneCameraIccPcs::Lab,
        signature => return Err(SceneCameraIccError::UnsupportedPcs { signature }),
    };

    let mut profile_id = [0u8; 16];
    profile_id.copy_from_slice(&bytes[PROFILE_ID_OFFSET..PROFILE_ID_OFFSET + 16]);

    Ok(ValidatedHeader {
        encoded_icc_version: read_be_u32(bytes, 8).expect("validated ICC header length"),
        profile_id: (profile_id != [0; 16]).then_some(profile_id),
        pcs,
    })
}

fn parse_image_state(
    bytes: &[u8],
    trusted_scene: bool,
) -> Result<SceneCameraIccImageState, SceneCameraIccError> {
    let tag_count = read_be_u32(bytes, ICC_HEADER_BYTES)
        .ok_or(SceneCameraIccError::MalformedTagTable)? as usize;
    let table_bytes = tag_count
        .checked_mul(ICC_TAG_ENTRY_BYTES)
        .and_then(|entries| entries.checked_add(ICC_MIN_BYTES))
        .ok_or(SceneCameraIccError::MalformedTagTable)?;
    if table_bytes > bytes.len() {
        return Err(SceneCameraIccError::MalformedTagTable);
    }

    let mut ciis_range = None;
    let mut payload_ranges = Vec::with_capacity(tag_count);
    for index in 0..tag_count {
        let entry = ICC_MIN_BYTES + index * ICC_TAG_ENTRY_BYTES;
        let signature = read_be_u32(bytes, entry).ok_or(SceneCameraIccError::MalformedTagTable)?;
        let offset =
            read_be_u32(bytes, entry + 4).ok_or(SceneCameraIccError::MalformedTagTable)? as usize;
        let size =
            read_be_u32(bytes, entry + 8).ok_or(SceneCameraIccError::MalformedTagTable)? as usize;
        let end = offset
            .checked_add(size)
            .ok_or(SceneCameraIccError::MalformedTagTable)?;
        if size == 0 || offset < table_bytes || offset % 4 != 0 || end > bytes.len() {
            return Err(SceneCameraIccError::MalformedTagTable);
        }

        // ICC permits multiple tag signatures to share exactly the same tag
        // element, but distinct elements must not overlap. Rejecting partial
        // overlap here keeps the raw provenance scan deterministic before the
        // profile is handed to Little CMS.
        for &(previous_offset, previous_end) in &payload_ranges {
            let exact_shared = offset == previous_offset && end == previous_end;
            let overlaps = offset < previous_end && previous_offset < end;
            if overlaps && !exact_shared {
                return Err(SceneCameraIccError::MalformedTagTable);
            }
        }
        payload_ranges.push((offset, end));

        if signature == CIIS_TAG {
            if ciis_range.replace((offset, size)).is_some() {
                return Err(SceneCameraIccError::MalformedImageState);
            }
        }
    }

    let Some((offset, size)) = ciis_range else {
        return if trusted_scene {
            Ok(SceneCameraIccImageState::TrustedSceneOverride)
        } else {
            Err(SceneCameraIccError::MissingImageState)
        };
    };

    if size != 12
        || four_cc(bytes, offset) != Some(SIGNATURE_TYPE)
        || bytes.get(offset + 4..offset + 8) != Some(&[0, 0, 0, 0])
    {
        return Err(SceneCameraIccError::MalformedImageState);
    }

    let signature = four_cc(bytes, offset + 8).ok_or(SceneCameraIccError::MalformedImageState)?;
    match u32::from_be_bytes(signature) {
        SCOE_STATE => Ok(SceneCameraIccImageState::SceneColorimetryEstimates),
        FPCE_STATE => Ok(SceneCameraIccImageState::FocalPlaneColorimetryEstimates),
        _ => Err(SceneCameraIccError::UnsupportedImageState { signature }),
    }
}

fn linear_prophoto_profile(
    context: &ThreadContext,
) -> Result<Profile<ThreadContext>, lcms2::Error> {
    let white = CIExyY {
        x: 0.3457,
        y: 0.3585,
        Y: 1.0,
    };
    let primaries = CIExyYTRIPLE {
        Red: CIExyY {
            x: 0.7347,
            y: 0.2653,
            Y: 1.0,
        },
        Green: CIExyY {
            x: 0.1596,
            y: 0.8404,
            Y: 1.0,
        },
        Blue: CIExyY {
            x: 0.0366,
            y: 0.0001,
            Y: 1.0,
        },
    };
    let linear = ToneCurve::new(1.0);
    Profile::new_rgb_context(context, &white, &primaries, &[&linear, &linear, &linear])
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_be_bytes(value))
}

fn four_cc(bytes: &[u8], offset: usize) -> Option<[u8; 4]> {
    bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()
}

struct FourCc([u8; 4]);

impl fmt::Display for FourCc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("'")?;
        for byte in self.0 {
            for escaped in std::ascii::escape_default(byte) {
                f.write_str(&(escaped as char).to_string())?;
            }
        }
        f.write_str("'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foreign_types_shared::ForeignType;
    use lcms2::{ColorSpaceSignature, Pipeline, ProfileClassSignature, Stage, Tag};
    use lcms2_sys::ColorimetricIntentImageState;

    // ProPhoto-to-XYZ with the red and blue input columns exchanged. It keeps
    // the same D50 white but makes LUT selection observable on RGB primaries.
    const SWAPPED_PROPHOTO_TO_XYZ_D50: [f64; 9] = [
        0.031_353_4,
        0.135_191_7,
        0.797_674_9,
        0.000_085_7,
        0.711_874_1,
        0.288_040_2,
        0.825_21,
        0.0,
        0.0,
    ];

    fn prophoto_profile(
        class: ProfileClassSignature,
        state: Option<ColorimetricIntentImageState>,
        gamma: f64,
    ) -> Vec<u8> {
        let white = CIExyY {
            x: 0.3457,
            y: 0.3585,
            Y: 1.0,
        };
        let primaries = CIExyYTRIPLE {
            Red: CIExyY {
                x: 0.7347,
                y: 0.2653,
                Y: 1.0,
            },
            Green: CIExyY {
                x: 0.1596,
                y: 0.8404,
                Y: 1.0,
            },
            Blue: CIExyY {
                x: 0.0366,
                y: 0.0001,
                Y: 1.0,
            },
        };
        let curve = ToneCurve::new(gamma);
        let mut profile = Profile::new_rgb(&white, &primaries, &[&curve, &curve, &curve])
            .expect("self-authored RGB profile");
        profile.set_version(4.3);
        profile.set_device_class(class);
        if let Some(state) = state {
            assert!(profile.write_tag(
                TagSignature::ColorimetricIntentImageStateTag,
                Tag::ColorimetricIntentImageState(state),
            ));
        }
        profile.icc().expect("serialize self-authored profile")
    }

    fn scene_profile(state: Option<ColorimetricIntentImageState>) -> Vec<u8> {
        prophoto_profile(ProfileClassSignature::InputClass, state, 1.0)
    }

    fn append_stage(pipeline: &Pipeline, stage: Stage) {
        let stage = stage.into_ptr();
        let inserted = unsafe {
            lcms2_sys::cmsPipelineInsertStage(pipeline.as_ptr(), lcms2_sys::StageLoc::AT_END, stage)
        };
        if inserted == 0 {
            unsafe { lcms2_sys::cmsStageFree(stage) };
            panic!("self-authored ICC pipeline rejected a compatible stage");
        }
    }

    fn swapped_prophoto_to_xyz_lut() -> Pipeline {
        let input_curve = ToneCurve::new(1.0);
        let output_curve = ToneCurve::new(1.0);
        let input_curves =
            Stage::new_tone_curves(&[&input_curve, &input_curve, &input_curve]).unwrap();
        let matrix = Stage::new_matrix(&SWAPPED_PROPHOTO_TO_XYZ_D50, 3, 3, None).unwrap();
        let output_curves =
            Stage::new_tone_curves(&[&output_curve, &output_curve, &output_curve]).unwrap();
        let pipeline = Pipeline::new(3, 3).unwrap();
        append_stage(&pipeline, input_curves);
        append_stage(&pipeline, matrix);
        append_stage(&pipeline, output_curves);
        pipeline
    }

    fn scene_profile_with_atob0() -> Vec<u8> {
        let bytes = scene_profile(Some(
            ColorimetricIntentImageState::SceneColorimetryEstimates,
        ));
        let mut profile = Profile::new_icc(&bytes).unwrap();
        let pipeline = swapped_prophoto_to_xyz_lut();
        assert!(profile.write_tag(TagSignature::AToB0Tag, Tag::Pipeline(&pipeline)));
        profile.icc().unwrap()
    }

    fn ciis_entry(bytes: &[u8]) -> (usize, usize) {
        let count = read_be_u32(bytes, ICC_HEADER_BYTES).unwrap() as usize;
        for index in 0..count {
            let entry = ICC_MIN_BYTES + index * ICC_TAG_ENTRY_BYTES;
            if read_be_u32(bytes, entry) == Some(CIIS_TAG) {
                return (entry, read_be_u32(bytes, entry + 4).unwrap() as usize);
            }
        }
        panic!("self-authored profile has no ciis tag")
    }

    fn tag_records(bytes: &[u8]) -> Vec<(usize, usize, usize)> {
        let count = read_be_u32(bytes, ICC_HEADER_BYTES).unwrap() as usize;
        (0..count)
            .map(|index| {
                let entry = ICC_MIN_BYTES + index * ICC_TAG_ENTRY_BYTES;
                (
                    entry,
                    read_be_u32(bytes, entry + 4).unwrap() as usize,
                    read_be_u32(bytes, entry + 8).unwrap() as usize,
                )
            })
            .collect()
    }

    fn assert_close(actual: [f32; 3], expected: [f32; 3]) {
        for channel in 0..3 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= 5.0e-4,
                "channel {channel}: actual {}, expected {}",
                actual[channel],
                expected[channel]
            );
        }
    }

    #[test]
    fn relative_encoding_selection_matches_lcms_precedence_and_scope() {
        use SceneCameraIccEncoding::{AToB0Fallback, AToB1, DToB1, MatrixShaper};

        // Relative-only LUTs do not make an otherwise unsupported profile
        // eligible: this module requires the input baseline or matrix/shaper.
        assert_eq!(select_relative_encoding(false, false, false, false), None);
        assert_eq!(select_relative_encoding(true, true, false, false), None);

        assert_eq!(
            select_relative_encoding(false, false, false, true),
            Some(MatrixShaper)
        );
        assert_eq!(
            select_relative_encoding(false, false, true, false),
            Some(AToB0Fallback)
        );
        assert_eq!(
            select_relative_encoding(false, true, true, true),
            Some(AToB1)
        );
        assert_eq!(
            select_relative_encoding(true, true, true, true),
            Some(DToB1)
        );
    }

    #[test]
    fn real_atob0_overrides_matrix_shaper_for_relative_transform() {
        let bytes = scene_profile_with_atob0();
        let converter = SceneCameraIcc::new(&bytes, false).unwrap();

        assert_eq!(
            converter.metadata().encoding,
            SceneCameraIccEncoding::AToB0Fallback
        );
        let swapped = converter.convert_rgb([1.0, 0.0, 0.0]).unwrap();
        assert!(
            swapped[0].abs() < 1.0e-3,
            "red was not swapped: {swapped:?}"
        );
        assert!(swapped[1].abs() < 1.0e-3, "green leaked: {swapped:?}");
        assert!(
            swapped[2] > 1.5,
            "AToB0 was not selected over the identity matrix path: {swapped:?}"
        );
    }

    #[test]
    fn scene_matrix_shaper_maps_basis_and_neutral_to_linear_prophoto() {
        let bytes = scene_profile(Some(
            ColorimetricIntentImageState::SceneColorimetryEstimates,
        ));
        let converter = SceneCameraIcc::new(&bytes, false).unwrap();

        assert_eq!(
            converter.metadata().encoding,
            SceneCameraIccEncoding::MatrixShaper
        );
        assert_eq!(converter.metadata().pcs, SceneCameraIccPcs::Xyz);
        assert_eq!(
            converter.metadata().image_state,
            SceneCameraIccImageState::SceneColorimetryEstimates
        );
        for value in [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.18, 0.18, 0.18],
        ] {
            assert_close(converter.convert_rgb(value).unwrap(), value);
        }
    }

    #[test]
    fn focal_plane_state_is_accepted() {
        let bytes = scene_profile(Some(
            ColorimetricIntentImageState::FocalPlaneColorimetryEstimates,
        ));
        let metadata = classify_scene_camera_icc(&bytes, false).unwrap();
        assert_eq!(
            metadata.image_state,
            SceneCameraIccImageState::FocalPlaneColorimetryEstimates
        );
    }

    #[test]
    fn missing_ciis_requires_explicit_trust() {
        let bytes = scene_profile(None);
        assert_eq!(
            SceneCameraIcc::new(&bytes, false).unwrap_err(),
            SceneCameraIccError::MissingImageState
        );
        let trusted = SceneCameraIcc::new(&bytes, true).unwrap();
        assert_eq!(
            trusted.metadata().image_state,
            SceneCameraIccImageState::TrustedSceneOverride
        );
    }

    #[test]
    fn wrong_ciis_is_rejected_even_when_caller_trusts_profile() {
        let bytes = scene_profile(Some(ColorimetricIntentImageState::SceneAppearanceEstimates));
        assert!(matches!(
            SceneCameraIcc::new(&bytes, true),
            Err(SceneCameraIccError::UnsupportedImageState {
                signature
            }) if signature == *b"sape"
        ));
    }

    #[test]
    fn malformed_ciis_is_rejected() {
        let mut bytes = scene_profile(Some(
            ColorimetricIntentImageState::SceneColorimetryEstimates,
        ));
        let (_, offset) = ciis_entry(&bytes);
        bytes[offset + 4] = 1;
        assert_eq!(
            SceneCameraIcc::new(&bytes, false).unwrap_err(),
            SceneCameraIccError::MalformedImageState
        );
    }

    #[test]
    fn misaligned_and_partially_overlapping_tag_payloads_are_rejected() {
        let bytes = scene_profile(Some(
            ColorimetricIntentImageState::SceneColorimetryEstimates,
        ));

        let mut misaligned = bytes.clone();
        let (ciis_entry, ciis_offset) = ciis_entry(&misaligned);
        misaligned[ciis_entry + 4..ciis_entry + 8]
            .copy_from_slice(&((ciis_offset + 1) as u32).to_be_bytes());
        assert_eq!(
            SceneCameraIcc::new(&misaligned, false).unwrap_err(),
            SceneCameraIccError::MalformedTagTable
        );

        let records = tag_records(&bytes);
        let (first, second) = records
            .iter()
            .enumerate()
            .flat_map(|(first_index, first)| {
                records
                    .iter()
                    .skip(first_index + 1)
                    .map(move |second| (first, second))
            })
            .find(|((_, first_offset, first_size), (_, _, second_size))| {
                first_size != second_size
                    && first_offset
                        .checked_add(*second_size)
                        .is_some_and(|end| end <= bytes.len())
            })
            .expect("self-authored profile has differently sized tag payloads");
        let mut overlapping = bytes;
        overlapping[second.0 + 4..second.0 + 8].copy_from_slice(&(first.1 as u32).to_be_bytes());
        assert_eq!(
            SceneCameraIcc::new(&overlapping, false).unwrap_err(),
            SceneCameraIccError::MalformedTagTable
        );
    }

    #[test]
    fn display_and_output_profiles_are_rejected() {
        for class in [
            ProfileClassSignature::DisplayClass,
            ProfileClassSignature::OutputClass,
        ] {
            let bytes = prophoto_profile(
                class,
                Some(ColorimetricIntentImageState::SceneColorimetryEstimates),
                1.0,
            );
            assert!(matches!(
                SceneCameraIcc::new(&bytes, false),
                Err(SceneCameraIccError::UnsupportedDeviceClass { .. })
            ));
        }
    }

    #[test]
    fn cmyk_header_is_rejected_before_lcms_execution() {
        let mut bytes = scene_profile(Some(
            ColorimetricIntentImageState::SceneColorimetryEstimates,
        ));
        bytes[DATA_COLOR_SPACE_OFFSET..DATA_COLOR_SPACE_OFFSET + 4]
            .copy_from_slice(&(ColorSpaceSignature::CmykData as u32).to_be_bytes());
        assert_eq!(
            SceneCameraIcc::new(&bytes, false).unwrap_err(),
            SceneCameraIccError::UnsupportedColorSpace {
                signature: *b"CMYK"
            }
        );
    }

    #[test]
    fn signed_hdr_and_non_finite_input_is_never_silently_clamped() {
        let bytes = scene_profile(Some(
            ColorimetricIntentImageState::SceneColorimetryEstimates,
        ));
        let converter = SceneCameraIcc::new(&bytes, false).unwrap();

        for (rgb, channel) in [([-0.01, 0.2, 0.3], 0), ([0.2, 1.01, 0.3], 1)] {
            assert!(matches!(
                converter.convert_rgb(rgb),
                Err(SceneCameraIccError::InputOutOfRange {
                    pixel: 0,
                    channel: actual,
                    ..
                }) if actual == channel
            ));
        }
        assert!(matches!(
            converter.convert_rgb([0.2, 0.3, f32::NAN]),
            Err(SceneCameraIccError::NonFiniteInput {
                pixel: 0,
                channel: 2,
                ..
            })
        ));
    }

    #[test]
    fn lcms_gamma_curve_clips_negative_values_behind_the_guard() {
        let bytes = prophoto_profile(
            ProfileClassSignature::InputClass,
            Some(ColorimetricIntentImageState::SceneColorimetryEstimates),
            2.2,
        );
        let converter = SceneCameraIcc::new(&bytes, false).unwrap();
        let mut output = [[f32::NAN; 3]];

        // Direct access is test-only: this records why the public API rejects
        // signed input instead of presenting Little CMS clipping as valid HDR.
        converter
            .transform
            .transform_pixels(&[[-0.25, 0.0, 0.0]], &mut output);
        assert_close(output[0], [0.0, 0.0, 0.0]);
        assert!(matches!(
            converter.convert_rgb([-0.25, 0.0, 0.0]),
            Err(SceneCameraIccError::InputOutOfRange { .. })
        ));
    }

    #[test]
    fn converter_is_send_and_sync_without_unsafe_application_sharing() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SceneCameraIcc>();
    }
}
