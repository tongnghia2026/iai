//! Bounded, clean-room parser for standalone DNG camera profiles (`.dcp`).
//!
//! This module implements only the classic-TIFF camera-profile container and
//! the technical/creative tags needed by iAi. It deliberately does not apply a
//! profile to pixels. In particular, [`DcpCreativeMetadata`] is parsed for
//! provenance and future rendering work, but its look table and tone curve are
//! not part of scene-linear camera characterization.

use std::collections::HashSet;
use std::fmt;

const DCP_MAGIC: u16 = 0x4352;
const MAX_DCP_BYTES: usize = 64 * 1024 * 1024;
const MAX_IFD_ENTRIES: usize = 256;
const MAX_STRING_BYTES: usize = 64 * 1024;
const MAX_LUT_CELLS: usize = 1_048_576;
const MAX_TONE_CURVE_SAMPLES: usize = 65_536;

const TYPE_BYTE: u16 = 1;
const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_SRATIONAL: u16 = 10;
const TYPE_FLOAT: u16 = 11;

const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
const TAG_COLOR_MATRIX_1: u16 = 50721;
const TAG_COLOR_MATRIX_2: u16 = 50722;
const TAG_CALIBRATION_ILLUMINANT_1: u16 = 50778;
const TAG_CALIBRATION_ILLUMINANT_2: u16 = 50779;
const TAG_PROFILE_CALIBRATION_SIGNATURE: u16 = 50932;
const TAG_PROFILE_NAME: u16 = 50936;
const TAG_HUE_SAT_MAP_DIMS: u16 = 50937;
const TAG_HUE_SAT_MAP_DATA_1: u16 = 50938;
const TAG_HUE_SAT_MAP_DATA_2: u16 = 50939;
const TAG_PROFILE_TONE_CURVE: u16 = 50940;
const TAG_PROFILE_EMBED_POLICY: u16 = 50941;
const TAG_PROFILE_COPYRIGHT: u16 = 50942;
const TAG_FORWARD_MATRIX_1: u16 = 50964;
const TAG_FORWARD_MATRIX_2: u16 = 50965;
const TAG_LOOK_TABLE_DIMS: u16 = 50981;
const TAG_LOOK_TABLE_DATA: u16 = 50982;
const TAG_HUE_SAT_MAP_ENCODING: u16 = 51107;
const TAG_LOOK_TABLE_ENCODING: u16 = 51108;

// DNG 1.6 custom-illuminant payloads. The current slice has no spectral or xy
// custom-illuminant implementation, so recognizing these is safer than
// silently treating illuminant 255 as an ordinary EXIF light-source value.
const TAG_ILLUMINANT_DATA_1: u16 = 52533;
const TAG_ILLUMINANT_DATA_2: u16 = 52534;

// DNG 1.6 third-calibration tags. A later slice may implement the required
// three-way interpolation; this two-calibration parser rejects them explicitly.
const TAG_CALIBRATION_ILLUMINANT_3: u16 = 52529;
const TAG_CAMERA_CALIBRATION_3: u16 = 52530;
const TAG_COLOR_MATRIX_3: u16 = 52531;
const TAG_FORWARD_MATRIX_3: u16 = 52532;
const TAG_ILLUMINANT_DATA_3: u16 = 52535;
const TAG_HUE_SAT_MAP_DATA_3: u16 = 52537;
const TAG_REDUCTION_MATRIX_3: u16 = 52538;

/// Byte order declared by the standalone DCP container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DcpByteOrder {
    LittleEndian,
    BigEndian,
}

impl DcpByteOrder {
    fn read_u16(self, bytes: &[u8], at: usize) -> Result<u16, DcpError> {
        let raw = bytes
            .get(at..at.checked_add(2).ok_or(DcpError::Overflow)?)
            .ok_or(DcpError::Truncated)?;
        Ok(match self {
            Self::LittleEndian => u16::from_le_bytes([raw[0], raw[1]]),
            Self::BigEndian => u16::from_be_bytes([raw[0], raw[1]]),
        })
    }

    fn read_u32(self, bytes: &[u8], at: usize) -> Result<u32, DcpError> {
        let raw = bytes
            .get(at..at.checked_add(4).ok_or(DcpError::Overflow)?)
            .ok_or(DcpError::Truncated)?;
        Ok(match self {
            Self::LittleEndian => u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
            Self::BigEndian => u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]),
        })
    }

    fn read_i32(self, bytes: &[u8], at: usize) -> Result<i32, DcpError> {
        Ok(self.read_u32(bytes, at)? as i32)
    }

    fn read_f32(self, bytes: &[u8], at: usize) -> Result<f32, DcpError> {
        Ok(f32::from_bits(self.read_u32(bytes, at)?))
    }
}

/// Hue/saturation/value table dimensions, in DNG's H, S, V order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DcpTableDimensions {
    pub hue_divisions: u32,
    pub saturation_divisions: u32,
    pub value_divisions: u32,
}

/// One DNG hue/saturation/value correction triplet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DcpHsvAdjustment {
    pub hue_shift_degrees: f32,
    pub saturation_scale: f32,
    pub value_scale: f32,
}

/// Encoding used for the value coordinate of a 3D DNG table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DcpTableEncoding {
    Linear,
    Srgb,
}

/// A validated DNG H/S/V table.
///
/// Entries retain the specification's storage order: value is the outer loop,
/// hue the middle loop, and saturation the inner loop.
#[derive(Clone, Debug, PartialEq)]
pub struct DcpHsvTable {
    pub dimensions: DcpTableDimensions,
    pub encoding: DcpTableEncoding,
    pub entries: Vec<DcpHsvAdjustment>,
}

/// Usage policy declared by `ProfileEmbedPolicy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DcpEmbedPolicy {
    AllowCopying,
    EmbedIfUsed,
    EmbedNever,
    NoRestrictions,
}

/// One technical camera calibration supported by this parser.
#[derive(Clone, Debug, PartialEq)]
pub struct DcpCalibration {
    /// EXIF `LightSource` value. Zero is permitted only for a single profile.
    pub illuminant: u16,
    /// XYZ-to-camera matrix, stored in row-scan order by DNG.
    pub color_matrix: [[f64; 3]; 3],
    /// Optional white-balanced camera-to-XYZ-D50 matrix.
    pub forward_matrix: Option<[[f64; 3]; 3]>,
    /// Technical hue/saturation map for this calibration, if present.
    pub hue_sat_map: Option<DcpHsvTable>,
}

/// Creative profile data retained as metadata, not scene characterization.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DcpCreativeMetadata {
    /// Strictly increasing `(input, output)` samples in the unit square.
    pub tone_curve: Option<Vec<[f32; 2]>>,
    /// A creative look table. This parser never applies it to scene-linear data.
    pub look_table: Option<DcpHsvTable>,
}

/// A validated standalone RGB DNG camera profile.
#[derive(Clone, Debug, PartialEq)]
pub struct DcpProfile {
    pub byte_order: DcpByteOrder,
    pub unique_camera_model: Option<String>,
    pub profile_calibration_signature: Option<String>,
    pub profile_name: Option<String>,
    /// Exactly one or two records. Third/custom illuminants are rejected.
    pub calibrations: Vec<DcpCalibration>,
    pub embed_policy: DcpEmbedPolicy,
    pub copyright: Option<String>,
    pub creative: DcpCreativeMetadata,
}

/// Structural or semantic failure while parsing a standalone DCP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DcpError {
    FileTooLarge {
        actual: usize,
        maximum: usize,
    },
    Truncated,
    InvalidByteOrder,
    InvalidMagic {
        actual: u16,
    },
    InvalidIfdOffset {
        offset: u32,
    },
    TooManyIfdEntries {
        actual: usize,
        maximum: usize,
    },
    MultipleIfdsUnsupported {
        next_ifd_offset: u32,
    },
    DuplicateTag {
        tag: u16,
    },
    UnsupportedThirdCalibration {
        tag: u16,
    },
    UnsupportedCustomIlluminant {
        calibration: u8,
    },
    MissingRequiredTag {
        tag: u16,
    },
    IncompleteSecondCalibration,
    InvalidDualIlluminants,
    InvalidType {
        tag: u16,
        expected: &'static str,
        actual: u16,
    },
    InvalidCount {
        tag: u16,
        expected: &'static str,
        actual: u32,
    },
    ResourceLimit {
        tag: u16,
        actual: u64,
        maximum: u64,
    },
    ValueOutOfBounds {
        tag: u16,
    },
    InvalidUtf8 {
        tag: u16,
    },
    MissingStringTerminator {
        tag: u16,
    },
    ZeroDenominator {
        tag: u16,
        index: usize,
    },
    NonFinite {
        tag: u16,
        index: usize,
    },
    SingularMatrix {
        tag: u16,
    },
    InvalidTableDimensions {
        tag: u16,
    },
    LutCountMismatch {
        tag: u16,
        expected: usize,
        actual: usize,
    },
    InvalidZeroSaturationEntry {
        tag: u16,
        index: usize,
    },
    InvalidToneCurve,
    InvalidEncoding {
        tag: u16,
        value: u32,
    },
    InvalidEmbedPolicy {
        value: u32,
    },
    OrphanedTag {
        tag: u16,
    },
    Overflow,
}

impl fmt::Display for DcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileTooLarge { actual, maximum } => {
                write!(f, "DCP is {actual} bytes; maximum is {maximum}")
            }
            Self::Truncated => f.write_str("DCP is truncated"),
            Self::InvalidByteOrder => f.write_str("DCP has an invalid byte-order marker"),
            Self::InvalidMagic { actual } => write!(f, "invalid DCP magic 0x{actual:04x}"),
            Self::InvalidIfdOffset { offset } => write!(f, "invalid DCP IFD offset {offset}"),
            Self::TooManyIfdEntries { actual, maximum } => {
                write!(f, "DCP has {actual} IFD entries; maximum is {maximum}")
            }
            Self::MultipleIfdsUnsupported { next_ifd_offset } => {
                write!(
                    f,
                    "DCP IFD chain at offset {next_ifd_offset} is unsupported"
                )
            }
            Self::DuplicateTag { tag } => write!(f, "duplicate DCP tag {tag}"),
            Self::UnsupportedThirdCalibration { tag } => {
                write!(f, "DCP third-calibration tag {tag} is unsupported")
            }
            Self::UnsupportedCustomIlluminant { calibration } => {
                write!(f, "DCP custom illuminant {calibration} is unsupported")
            }
            Self::MissingRequiredTag { tag } => write!(f, "missing required DCP tag {tag}"),
            Self::IncompleteSecondCalibration => {
                f.write_str("DCP second calibration is incomplete")
            }
            Self::InvalidDualIlluminants => f.write_str("DCP dual illuminants must both be known"),
            Self::InvalidType {
                tag,
                expected,
                actual,
            } => write!(f, "DCP tag {tag} has type {actual}; expected {expected}"),
            Self::InvalidCount {
                tag,
                expected,
                actual,
            } => write!(f, "DCP tag {tag} has count {actual}; expected {expected}"),
            Self::ResourceLimit {
                tag,
                actual,
                maximum,
            } => write!(f, "DCP tag {tag} has {actual} items; maximum is {maximum}"),
            Self::ValueOutOfBounds { tag } => write!(f, "DCP tag {tag} points outside the file"),
            Self::InvalidUtf8 { tag } => write!(f, "DCP tag {tag} is not valid UTF-8"),
            Self::MissingStringTerminator { tag } => {
                write!(f, "DCP string tag {tag} is not correctly null-terminated")
            }
            Self::ZeroDenominator { tag, index } => {
                write!(f, "DCP tag {tag} rational {index} has denominator zero")
            }
            Self::NonFinite { tag, index } => {
                write!(f, "DCP tag {tag} value {index} is non-finite")
            }
            Self::SingularMatrix { tag } => write!(f, "DCP matrix tag {tag} is singular"),
            Self::InvalidTableDimensions { tag } => {
                write!(f, "DCP table dimensions in tag {tag} are invalid")
            }
            Self::LutCountMismatch {
                tag,
                expected,
                actual,
            } => write!(
                f,
                "DCP LUT tag {tag} has {actual} values; expected {expected}"
            ),
            Self::InvalidZeroSaturationEntry { tag, index } => write!(
                f,
                "DCP LUT tag {tag} zero-saturation entry {index} must have value scale 1"
            ),
            Self::InvalidToneCurve => f.write_str("DCP tone curve is invalid"),
            Self::InvalidEncoding { tag, value } => {
                write!(f, "DCP encoding tag {tag} has invalid value {value}")
            }
            Self::InvalidEmbedPolicy { value } => {
                write!(f, "DCP embed policy has invalid value {value}")
            }
            Self::OrphanedTag { tag } => write!(f, "DCP tag {tag} has no matching companion tag"),
            Self::Overflow => f.write_str("integer overflow while parsing DCP"),
        }
    }
}

impl std::error::Error for DcpError {}

#[derive(Clone, Copy, Debug)]
struct IfdEntry {
    tag: u16,
    field_type: u16,
    count: u32,
    value_field_at: usize,
}

impl IfdEntry {
    fn value_bytes<'a>(self, bytes: &'a [u8], order: DcpByteOrder) -> Result<&'a [u8], DcpError> {
        let element_bytes = match self.field_type {
            TYPE_BYTE | TYPE_ASCII => 1usize,
            TYPE_SHORT => 2,
            TYPE_LONG | TYPE_FLOAT => 4,
            TYPE_SRATIONAL => 8,
            _ => {
                return Err(DcpError::InvalidType {
                    tag: self.tag,
                    expected: "a supported TIFF scalar type",
                    actual: self.field_type,
                });
            }
        };
        let count = usize::try_from(self.count).map_err(|_| DcpError::Overflow)?;
        let byte_len = count.checked_mul(element_bytes).ok_or(DcpError::Overflow)?;
        let start = if byte_len <= 4 {
            self.value_field_at
        } else {
            let offset = order.read_u32(bytes, self.value_field_at)?;
            usize::try_from(offset).map_err(|_| DcpError::Overflow)?
        };
        let end = start.checked_add(byte_len).ok_or(DcpError::Overflow)?;
        bytes
            .get(start..end)
            .ok_or(DcpError::ValueOutOfBounds { tag: self.tag })
    }
}

#[derive(Default)]
struct ParsedTags {
    unique_camera_model: Option<String>,
    profile_calibration_signature: Option<String>,
    profile_name: Option<String>,
    color_matrix_1: Option<[[f64; 3]; 3]>,
    color_matrix_2: Option<[[f64; 3]; 3]>,
    illuminant_1: Option<u16>,
    illuminant_2: Option<u16>,
    forward_matrix_1: Option<[[f64; 3]; 3]>,
    forward_matrix_2: Option<[[f64; 3]; 3]>,
    hue_sat_dims: Option<DcpTableDimensions>,
    hue_sat_data_1: Option<Vec<f32>>,
    hue_sat_data_2: Option<Vec<f32>>,
    hue_sat_encoding: Option<DcpTableEncoding>,
    tone_curve: Option<Vec<[f32; 2]>>,
    embed_policy: Option<DcpEmbedPolicy>,
    copyright: Option<String>,
    look_dims: Option<DcpTableDimensions>,
    look_data: Option<Vec<f32>>,
    look_encoding: Option<DcpTableEncoding>,
}

/// Parse a standalone RGB DNG camera profile from memory.
///
/// The parser accepts both TIFF byte orders, requires the standalone `CR`
/// magic, follows exactly one bounded IFD, and rejects unsupported third or
/// custom illuminants. Unknown IFD tags are skipped without dereferencing their
/// type, count, or offset.
pub fn parse(bytes: &[u8]) -> Result<DcpProfile, DcpError> {
    if bytes.len() > MAX_DCP_BYTES {
        return Err(DcpError::FileTooLarge {
            actual: bytes.len(),
            maximum: MAX_DCP_BYTES,
        });
    }
    if bytes.len() < 8 {
        return Err(DcpError::Truncated);
    }

    let order = match &bytes[0..2] {
        b"II" => DcpByteOrder::LittleEndian,
        b"MM" => DcpByteOrder::BigEndian,
        _ => return Err(DcpError::InvalidByteOrder),
    };
    let magic = order.read_u16(bytes, 2)?;
    if magic != DCP_MAGIC {
        return Err(DcpError::InvalidMagic { actual: magic });
    }

    let ifd_offset_u32 = order.read_u32(bytes, 4)?;
    let ifd_offset = usize::try_from(ifd_offset_u32).map_err(|_| DcpError::Overflow)?;
    if ifd_offset < 8 || ifd_offset >= bytes.len() {
        return Err(DcpError::InvalidIfdOffset {
            offset: ifd_offset_u32,
        });
    }
    let entry_count = usize::from(order.read_u16(bytes, ifd_offset)?);
    if entry_count > MAX_IFD_ENTRIES {
        return Err(DcpError::TooManyIfdEntries {
            actual: entry_count,
            maximum: MAX_IFD_ENTRIES,
        });
    }
    let entries_start = ifd_offset.checked_add(2).ok_or(DcpError::Overflow)?;
    let entries_bytes = entry_count.checked_mul(12).ok_or(DcpError::Overflow)?;
    let next_ifd_at = entries_start
        .checked_add(entries_bytes)
        .ok_or(DcpError::Overflow)?;
    let ifd_end = next_ifd_at.checked_add(4).ok_or(DcpError::Overflow)?;
    if ifd_end > bytes.len() {
        return Err(DcpError::Truncated);
    }

    let next_ifd_offset = order.read_u32(bytes, next_ifd_at)?;
    if next_ifd_offset != 0 {
        return Err(DcpError::MultipleIfdsUnsupported { next_ifd_offset });
    }

    let mut parsed = ParsedTags::default();
    let mut seen = HashSet::with_capacity(entry_count.min(32));
    for index in 0..entry_count {
        let entry_at = entries_start
            .checked_add(index.checked_mul(12).ok_or(DcpError::Overflow)?)
            .ok_or(DcpError::Overflow)?;
        let entry = IfdEntry {
            tag: order.read_u16(bytes, entry_at)?,
            field_type: order.read_u16(bytes, entry_at + 2)?,
            count: order.read_u32(bytes, entry_at + 4)?,
            value_field_at: entry_at + 8,
        };

        if is_third_calibration_tag(entry.tag) {
            return Err(DcpError::UnsupportedThirdCalibration { tag: entry.tag });
        }
        if matches!(entry.tag, TAG_ILLUMINANT_DATA_1 | TAG_ILLUMINANT_DATA_2) {
            let calibration = if entry.tag == TAG_ILLUMINANT_DATA_1 {
                1
            } else {
                2
            };
            return Err(DcpError::UnsupportedCustomIlluminant { calibration });
        }
        if !is_supported_tag(entry.tag) {
            continue;
        }
        if !seen.insert(entry.tag) {
            return Err(DcpError::DuplicateTag { tag: entry.tag });
        }

        match entry.tag {
            TAG_UNIQUE_CAMERA_MODEL => {
                parsed.unique_camera_model = Some(parse_string(entry, bytes, order, false)?);
            }
            TAG_PROFILE_CALIBRATION_SIGNATURE => {
                parsed.profile_calibration_signature =
                    Some(parse_string(entry, bytes, order, true)?);
            }
            TAG_PROFILE_NAME => {
                parsed.profile_name = Some(parse_string(entry, bytes, order, true)?);
            }
            TAG_PROFILE_COPYRIGHT => {
                parsed.copyright = Some(parse_string(entry, bytes, order, true)?);
            }
            TAG_COLOR_MATRIX_1 => {
                parsed.color_matrix_1 = Some(parse_matrix(entry, bytes, order)?);
            }
            TAG_COLOR_MATRIX_2 => {
                parsed.color_matrix_2 = Some(parse_matrix(entry, bytes, order)?);
            }
            TAG_FORWARD_MATRIX_1 => {
                parsed.forward_matrix_1 = Some(parse_matrix(entry, bytes, order)?);
            }
            TAG_FORWARD_MATRIX_2 => {
                parsed.forward_matrix_2 = Some(parse_matrix(entry, bytes, order)?);
            }
            TAG_CALIBRATION_ILLUMINANT_1 => {
                parsed.illuminant_1 = Some(parse_short(entry, bytes, order)?);
            }
            TAG_CALIBRATION_ILLUMINANT_2 => {
                parsed.illuminant_2 = Some(parse_short(entry, bytes, order)?);
            }
            TAG_HUE_SAT_MAP_DIMS => {
                parsed.hue_sat_dims = Some(parse_dimensions(entry, bytes, order)?);
            }
            TAG_HUE_SAT_MAP_DATA_1 => {
                parsed.hue_sat_data_1 =
                    Some(parse_float_values(entry, bytes, order, MAX_LUT_CELLS * 3)?);
            }
            TAG_HUE_SAT_MAP_DATA_2 => {
                parsed.hue_sat_data_2 =
                    Some(parse_float_values(entry, bytes, order, MAX_LUT_CELLS * 3)?);
            }
            TAG_HUE_SAT_MAP_ENCODING => {
                parsed.hue_sat_encoding = Some(parse_encoding(entry, bytes, order)?);
            }
            TAG_PROFILE_TONE_CURVE => {
                parsed.tone_curve = Some(parse_tone_curve(entry, bytes, order)?);
            }
            TAG_PROFILE_EMBED_POLICY => {
                parsed.embed_policy = Some(parse_embed_policy(entry, bytes, order)?);
            }
            TAG_LOOK_TABLE_DIMS => {
                parsed.look_dims = Some(parse_dimensions(entry, bytes, order)?);
            }
            TAG_LOOK_TABLE_DATA => {
                parsed.look_data =
                    Some(parse_float_values(entry, bytes, order, MAX_LUT_CELLS * 3)?);
            }
            TAG_LOOK_TABLE_ENCODING => {
                parsed.look_encoding = Some(parse_encoding(entry, bytes, order)?);
            }
            _ => unreachable!("supported DCP tag was not handled"),
        }
    }

    finish_profile(order, parsed)
}

fn finish_profile(order: DcpByteOrder, mut parsed: ParsedTags) -> Result<DcpProfile, DcpError> {
    let color_matrix_1 = parsed
        .color_matrix_1
        .take()
        .ok_or(DcpError::MissingRequiredTag {
            tag: TAG_COLOR_MATRIX_1,
        })?;
    let illuminant_1 = parsed.illuminant_1.unwrap_or(0);
    if illuminant_1 == 255 {
        return Err(DcpError::UnsupportedCustomIlluminant { calibration: 1 });
    }
    if parsed.illuminant_2 == Some(255) {
        return Err(DcpError::UnsupportedCustomIlluminant { calibration: 2 });
    }

    let second_present = parsed.color_matrix_2.is_some()
        || parsed.illuminant_2.is_some()
        || parsed.forward_matrix_2.is_some()
        || parsed.hue_sat_data_2.is_some();
    if second_present && (parsed.color_matrix_2.is_none() || parsed.illuminant_2.is_none()) {
        return Err(DcpError::IncompleteSecondCalibration);
    }
    if second_present && (illuminant_1 == 0 || parsed.illuminant_2 == Some(0)) {
        return Err(DcpError::InvalidDualIlluminants);
    }
    if second_present && (parsed.forward_matrix_1.is_some() != parsed.forward_matrix_2.is_some()) {
        return Err(DcpError::IncompleteSecondCalibration);
    }

    let hue_tables_present = parsed.hue_sat_data_1.is_some() || parsed.hue_sat_data_2.is_some();
    if parsed.hue_sat_dims.is_some() != hue_tables_present {
        return Err(DcpError::OrphanedTag {
            tag: if parsed.hue_sat_dims.is_some() {
                TAG_HUE_SAT_MAP_DIMS
            } else {
                TAG_HUE_SAT_MAP_DATA_1
            },
        });
    }
    if parsed.hue_sat_data_2.is_some() && parsed.hue_sat_data_1.is_none() {
        return Err(DcpError::OrphanedTag {
            tag: TAG_HUE_SAT_MAP_DATA_2,
        });
    }
    if parsed.hue_sat_encoding.is_some() && !hue_tables_present {
        return Err(DcpError::OrphanedTag {
            tag: TAG_HUE_SAT_MAP_ENCODING,
        });
    }

    let hue_encoding = parsed.hue_sat_encoding.unwrap_or(DcpTableEncoding::Linear);
    let hue_sat_map_1 = match (parsed.hue_sat_dims, parsed.hue_sat_data_1.take()) {
        (Some(dimensions), Some(values)) => Some(build_table(
            TAG_HUE_SAT_MAP_DATA_1,
            dimensions,
            hue_encoding,
            values,
        )?),
        _ => None,
    };
    let hue_sat_map_2 = match (parsed.hue_sat_dims, parsed.hue_sat_data_2.take()) {
        (Some(dimensions), Some(values)) => Some(build_table(
            TAG_HUE_SAT_MAP_DATA_2,
            dimensions,
            hue_encoding,
            values,
        )?),
        _ => None,
    };

    if parsed.look_dims.is_some() != parsed.look_data.is_some() {
        return Err(DcpError::OrphanedTag {
            tag: if parsed.look_dims.is_some() {
                TAG_LOOK_TABLE_DIMS
            } else {
                TAG_LOOK_TABLE_DATA
            },
        });
    }
    if parsed.look_encoding.is_some() && parsed.look_data.is_none() {
        return Err(DcpError::OrphanedTag {
            tag: TAG_LOOK_TABLE_ENCODING,
        });
    }
    let look_table = match (parsed.look_dims, parsed.look_data.take()) {
        (Some(dimensions), Some(values)) => Some(build_table(
            TAG_LOOK_TABLE_DATA,
            dimensions,
            parsed.look_encoding.unwrap_or(DcpTableEncoding::Linear),
            values,
        )?),
        _ => None,
    };

    let mut calibrations = Vec::with_capacity(if second_present { 2 } else { 1 });
    calibrations.push(DcpCalibration {
        illuminant: illuminant_1,
        color_matrix: color_matrix_1,
        forward_matrix: parsed.forward_matrix_1,
        hue_sat_map: hue_sat_map_1,
    });
    if second_present {
        calibrations.push(DcpCalibration {
            illuminant: parsed
                .illuminant_2
                .expect("second illuminant was validated"),
            color_matrix: parsed.color_matrix_2.expect("second matrix was validated"),
            forward_matrix: parsed.forward_matrix_2,
            hue_sat_map: hue_sat_map_2,
        });
    }

    Ok(DcpProfile {
        byte_order: order,
        unique_camera_model: parsed.unique_camera_model,
        profile_calibration_signature: parsed.profile_calibration_signature,
        profile_name: parsed.profile_name,
        calibrations,
        embed_policy: parsed.embed_policy.unwrap_or(DcpEmbedPolicy::AllowCopying),
        copyright: parsed.copyright,
        creative: DcpCreativeMetadata {
            tone_curve: parsed.tone_curve,
            look_table,
        },
    })
}

fn parse_string(
    entry: IfdEntry,
    bytes: &[u8],
    order: DcpByteOrder,
    allow_byte: bool,
) -> Result<String, DcpError> {
    let valid_type =
        entry.field_type == TYPE_ASCII || (allow_byte && entry.field_type == TYPE_BYTE);
    if !valid_type {
        return Err(DcpError::InvalidType {
            tag: entry.tag,
            expected: if allow_byte { "ASCII or BYTE" } else { "ASCII" },
            actual: entry.field_type,
        });
    }
    let count = usize::try_from(entry.count).map_err(|_| DcpError::Overflow)?;
    if count == 0 {
        return Err(DcpError::InvalidCount {
            tag: entry.tag,
            expected: "at least one byte including null",
            actual: entry.count,
        });
    }
    if count > MAX_STRING_BYTES {
        return Err(DcpError::ResourceLimit {
            tag: entry.tag,
            actual: count as u64,
            maximum: MAX_STRING_BYTES as u64,
        });
    }
    let raw = entry.value_bytes(bytes, order)?;
    if raw.last() != Some(&0) || raw[..raw.len() - 1].contains(&0) {
        return Err(DcpError::MissingStringTerminator { tag: entry.tag });
    }
    std::str::from_utf8(&raw[..raw.len() - 1])
        .map(str::to_owned)
        .map_err(|_| DcpError::InvalidUtf8 { tag: entry.tag })
}

fn parse_short(entry: IfdEntry, bytes: &[u8], order: DcpByteOrder) -> Result<u16, DcpError> {
    expect_type(entry, TYPE_SHORT, "SHORT")?;
    expect_count(entry, 1, "1")?;
    let raw = entry.value_bytes(bytes, order)?;
    order.read_u16(raw, 0)
}

fn parse_long(entry: IfdEntry, bytes: &[u8], order: DcpByteOrder) -> Result<u32, DcpError> {
    expect_type(entry, TYPE_LONG, "LONG")?;
    expect_count(entry, 1, "1")?;
    let raw = entry.value_bytes(bytes, order)?;
    order.read_u32(raw, 0)
}

fn parse_dimensions(
    entry: IfdEntry,
    bytes: &[u8],
    order: DcpByteOrder,
) -> Result<DcpTableDimensions, DcpError> {
    expect_type(entry, TYPE_LONG, "LONG")?;
    expect_count(entry, 3, "3")?;
    let raw = entry.value_bytes(bytes, order)?;
    let dimensions = DcpTableDimensions {
        hue_divisions: order.read_u32(raw, 0)?,
        saturation_divisions: order.read_u32(raw, 4)?,
        value_divisions: order.read_u32(raw, 8)?,
    };
    table_cell_count(entry.tag, dimensions)?;
    Ok(dimensions)
}

fn parse_matrix(
    entry: IfdEntry,
    bytes: &[u8],
    order: DcpByteOrder,
) -> Result<[[f64; 3]; 3], DcpError> {
    expect_type(entry, TYPE_SRATIONAL, "SRATIONAL")?;
    expect_count(entry, 9, "9 (an RGB 3x3 matrix)")?;
    let raw = entry.value_bytes(bytes, order)?;
    let mut values = [0.0f64; 9];
    for (index, value) in values.iter_mut().enumerate() {
        let at = index.checked_mul(8).ok_or(DcpError::Overflow)?;
        let numerator = order.read_i32(raw, at)?;
        let denominator = order.read_i32(raw, at + 4)?;
        if denominator == 0 {
            return Err(DcpError::ZeroDenominator {
                tag: entry.tag,
                index,
            });
        }
        *value = f64::from(numerator) / f64::from(denominator);
    }
    let matrix = [
        [values[0], values[1], values[2]],
        [values[3], values[4], values[5]],
        [values[6], values[7], values[8]],
    ];
    validate_matrix(entry.tag, matrix)?;
    Ok(matrix)
}

fn validate_matrix(tag: u16, matrix: [[f64; 3]; 3]) -> Result<(), DcpError> {
    let determinant = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
    let scale = matrix
        .iter()
        .flatten()
        .fold(0.0f64, |largest, value| largest.max(value.abs()));
    let singular_threshold = scale.powi(3) * 1.0e-12;
    if !determinant.is_finite() || determinant.abs() <= singular_threshold {
        return Err(DcpError::SingularMatrix { tag });
    }
    Ok(())
}

fn parse_float_values(
    entry: IfdEntry,
    bytes: &[u8],
    order: DcpByteOrder,
    maximum: usize,
) -> Result<Vec<f32>, DcpError> {
    expect_type(entry, TYPE_FLOAT, "FLOAT")?;
    let count = usize::try_from(entry.count).map_err(|_| DcpError::Overflow)?;
    if count > maximum {
        return Err(DcpError::ResourceLimit {
            tag: entry.tag,
            actual: count as u64,
            maximum: maximum as u64,
        });
    }
    let raw = entry.value_bytes(bytes, order)?;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let at = index.checked_mul(4).ok_or(DcpError::Overflow)?;
        let value = order.read_f32(raw, at)?;
        if !value.is_finite() {
            return Err(DcpError::NonFinite {
                tag: entry.tag,
                index,
            });
        }
        values.push(value);
    }
    Ok(values)
}

fn parse_tone_curve(
    entry: IfdEntry,
    bytes: &[u8],
    order: DcpByteOrder,
) -> Result<Vec<[f32; 2]>, DcpError> {
    if entry.count < 4 || entry.count % 2 != 0 {
        return Err(DcpError::InvalidCount {
            tag: entry.tag,
            expected: "an even count containing at least two points",
            actual: entry.count,
        });
    }
    let values = parse_float_values(entry, bytes, order, MAX_TONE_CURVE_SAMPLES * 2)?;
    let mut points = Vec::with_capacity(values.len() / 2);
    for pair in values.chunks_exact(2) {
        let point = [pair[0], pair[1]];
        if !(0.0..=1.0).contains(&point[0]) || !(0.0..=1.0).contains(&point[1]) {
            return Err(DcpError::InvalidToneCurve);
        }
        if points
            .last()
            .is_some_and(|previous: &[f32; 2]| point[0] <= previous[0])
        {
            return Err(DcpError::InvalidToneCurve);
        }
        points.push(point);
    }
    Ok(points)
}

fn parse_encoding(
    entry: IfdEntry,
    bytes: &[u8],
    order: DcpByteOrder,
) -> Result<DcpTableEncoding, DcpError> {
    match parse_long(entry, bytes, order)? {
        0 => Ok(DcpTableEncoding::Linear),
        1 => Ok(DcpTableEncoding::Srgb),
        value => Err(DcpError::InvalidEncoding {
            tag: entry.tag,
            value,
        }),
    }
}

fn parse_embed_policy(
    entry: IfdEntry,
    bytes: &[u8],
    order: DcpByteOrder,
) -> Result<DcpEmbedPolicy, DcpError> {
    match parse_long(entry, bytes, order)? {
        0 => Ok(DcpEmbedPolicy::AllowCopying),
        1 => Ok(DcpEmbedPolicy::EmbedIfUsed),
        2 => Ok(DcpEmbedPolicy::EmbedNever),
        3 => Ok(DcpEmbedPolicy::NoRestrictions),
        value => Err(DcpError::InvalidEmbedPolicy { value }),
    }
}

fn build_table(
    tag: u16,
    dimensions: DcpTableDimensions,
    encoding: DcpTableEncoding,
    values: Vec<f32>,
) -> Result<DcpHsvTable, DcpError> {
    let cells = table_cell_count(tag, dimensions)?;
    let expected = cells.checked_mul(3).ok_or(DcpError::Overflow)?;
    if values.len() != expected {
        return Err(DcpError::LutCountMismatch {
            tag,
            expected,
            actual: values.len(),
        });
    }
    let entries: Vec<DcpHsvAdjustment> = values
        .chunks_exact(3)
        .map(|entry| DcpHsvAdjustment {
            hue_shift_degrees: entry[0],
            saturation_scale: entry[1],
            value_scale: entry[2],
        })
        .collect();

    let saturation_divisions =
        usize::try_from(dimensions.saturation_divisions).map_err(|_| DcpError::Overflow)?;
    for (index, entry) in entries.iter().enumerate() {
        if index % saturation_divisions == 0 && entry.value_scale != 1.0 {
            return Err(DcpError::InvalidZeroSaturationEntry { tag, index });
        }
    }

    Ok(DcpHsvTable {
        dimensions,
        encoding,
        entries,
    })
}

fn table_cell_count(tag: u16, dimensions: DcpTableDimensions) -> Result<usize, DcpError> {
    if dimensions.hue_divisions < 1
        || dimensions.saturation_divisions < 2
        || dimensions.value_divisions < 1
    {
        return Err(DcpError::InvalidTableDimensions { tag });
    }
    let hue = usize::try_from(dimensions.hue_divisions).map_err(|_| DcpError::Overflow)?;
    let saturation =
        usize::try_from(dimensions.saturation_divisions).map_err(|_| DcpError::Overflow)?;
    let value = usize::try_from(dimensions.value_divisions).map_err(|_| DcpError::Overflow)?;
    let cells = hue
        .checked_mul(saturation)
        .and_then(|product| product.checked_mul(value))
        .ok_or(DcpError::Overflow)?;
    if cells > MAX_LUT_CELLS {
        return Err(DcpError::ResourceLimit {
            tag,
            actual: cells as u64,
            maximum: MAX_LUT_CELLS as u64,
        });
    }
    Ok(cells)
}

fn expect_type(entry: IfdEntry, expected: u16, name: &'static str) -> Result<(), DcpError> {
    if entry.field_type != expected {
        return Err(DcpError::InvalidType {
            tag: entry.tag,
            expected: name,
            actual: entry.field_type,
        });
    }
    Ok(())
}

fn expect_count(entry: IfdEntry, expected: u32, description: &'static str) -> Result<(), DcpError> {
    if entry.count != expected {
        return Err(DcpError::InvalidCount {
            tag: entry.tag,
            expected: description,
            actual: entry.count,
        });
    }
    Ok(())
}

fn is_supported_tag(tag: u16) -> bool {
    matches!(
        tag,
        TAG_UNIQUE_CAMERA_MODEL
            | TAG_COLOR_MATRIX_1
            | TAG_COLOR_MATRIX_2
            | TAG_CALIBRATION_ILLUMINANT_1
            | TAG_CALIBRATION_ILLUMINANT_2
            | TAG_PROFILE_CALIBRATION_SIGNATURE
            | TAG_PROFILE_NAME
            | TAG_HUE_SAT_MAP_DIMS
            | TAG_HUE_SAT_MAP_DATA_1
            | TAG_HUE_SAT_MAP_DATA_2
            | TAG_PROFILE_TONE_CURVE
            | TAG_PROFILE_EMBED_POLICY
            | TAG_PROFILE_COPYRIGHT
            | TAG_FORWARD_MATRIX_1
            | TAG_FORWARD_MATRIX_2
            | TAG_LOOK_TABLE_DIMS
            | TAG_LOOK_TABLE_DATA
            | TAG_HUE_SAT_MAP_ENCODING
            | TAG_LOOK_TABLE_ENCODING
    )
}

fn is_third_calibration_tag(tag: u16) -> bool {
    matches!(
        tag,
        TAG_CALIBRATION_ILLUMINANT_3
            | TAG_CAMERA_CALIBRATION_3
            | TAG_COLOR_MATRIX_3
            | TAG_FORWARD_MATRIX_3
            | TAG_ILLUMINANT_DATA_3
            | TAG_HUE_SAT_MAP_DATA_3
            | TAG_REDUCTION_MATRIX_3
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct TestEntry {
        tag: u16,
        field_type: u16,
        count: u32,
        value: Vec<u8>,
    }

    fn encode_u16(order: DcpByteOrder, value: u16) -> [u8; 2] {
        match order {
            DcpByteOrder::LittleEndian => value.to_le_bytes(),
            DcpByteOrder::BigEndian => value.to_be_bytes(),
        }
    }

    fn encode_u32(order: DcpByteOrder, value: u32) -> [u8; 4] {
        match order {
            DcpByteOrder::LittleEndian => value.to_le_bytes(),
            DcpByteOrder::BigEndian => value.to_be_bytes(),
        }
    }

    fn encode_i32(order: DcpByteOrder, value: i32) -> [u8; 4] {
        encode_u32(order, value as u32)
    }

    fn write_u16(bytes: &mut [u8], at: usize, order: DcpByteOrder, value: u16) {
        bytes[at..at + 2].copy_from_slice(&encode_u16(order, value));
    }

    fn write_u32(bytes: &mut [u8], at: usize, order: DcpByteOrder, value: u32) {
        bytes[at..at + 4].copy_from_slice(&encode_u32(order, value));
    }

    fn type_width(field_type: u16) -> Option<u64> {
        match field_type {
            TYPE_BYTE | TYPE_ASCII => Some(1),
            TYPE_SHORT => Some(2),
            TYPE_LONG | TYPE_FLOAT => Some(4),
            TYPE_SRATIONAL => Some(8),
            _ => None,
        }
    }

    fn build_dcp(order: DcpByteOrder, entries: Vec<TestEntry>) -> Vec<u8> {
        let ifd_start = 8usize;
        let data_start = ifd_start + 2 + entries.len() * 12 + 4;
        let mut bytes = vec![0u8; data_start];
        match order {
            DcpByteOrder::LittleEndian => bytes[0..2].copy_from_slice(b"II"),
            DcpByteOrder::BigEndian => bytes[0..2].copy_from_slice(b"MM"),
        }
        write_u16(&mut bytes, 2, order, DCP_MAGIC);
        write_u32(&mut bytes, 4, order, ifd_start as u32);
        write_u16(&mut bytes, ifd_start, order, entries.len() as u16);

        let mut payload = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            let at = ifd_start + 2 + index * 12;
            write_u16(&mut bytes, at, order, entry.tag);
            write_u16(&mut bytes, at + 2, order, entry.field_type);
            write_u32(&mut bytes, at + 4, order, entry.count);
            let declared_bytes = type_width(entry.field_type)
                .and_then(|width| width.checked_mul(u64::from(entry.count)));
            if declared_bytes.is_some_and(|size| size <= 4) {
                let copy_len = entry.value.len().min(4);
                bytes[at + 8..at + 8 + copy_len].copy_from_slice(&entry.value[..copy_len]);
            } else {
                let offset = data_start + payload.len();
                write_u32(&mut bytes, at + 8, order, offset as u32);
                payload.extend_from_slice(&entry.value);
                if payload.len() % 2 != 0 {
                    payload.push(0);
                }
            }
        }
        bytes.extend_from_slice(&payload);
        bytes
    }

    fn ascii_entry(tag: u16, field_type: u16, value: &[u8]) -> TestEntry {
        TestEntry {
            tag,
            field_type,
            count: value.len() as u32,
            value: value.to_vec(),
        }
    }

    fn short_entry(order: DcpByteOrder, tag: u16, value: u16) -> TestEntry {
        TestEntry {
            tag,
            field_type: TYPE_SHORT,
            count: 1,
            value: encode_u16(order, value).to_vec(),
        }
    }

    fn long_entry(order: DcpByteOrder, tag: u16, value: u32) -> TestEntry {
        TestEntry {
            tag,
            field_type: TYPE_LONG,
            count: 1,
            value: encode_u32(order, value).to_vec(),
        }
    }

    fn longs_entry(order: DcpByteOrder, tag: u16, values: &[u32]) -> TestEntry {
        let mut value = Vec::with_capacity(values.len() * 4);
        for item in values {
            value.extend_from_slice(&encode_u32(order, *item));
        }
        TestEntry {
            tag,
            field_type: TYPE_LONG,
            count: values.len() as u32,
            value,
        }
    }

    fn floats_entry(order: DcpByteOrder, tag: u16, values: &[f32]) -> TestEntry {
        let mut value = Vec::with_capacity(values.len() * 4);
        for item in values {
            value.extend_from_slice(&encode_u32(order, item.to_bits()));
        }
        TestEntry {
            tag,
            field_type: TYPE_FLOAT,
            count: values.len() as u32,
            value,
        }
    }

    fn matrix_entry(order: DcpByteOrder, tag: u16, values: &[(i32, i32)]) -> TestEntry {
        let mut value = Vec::with_capacity(values.len() * 8);
        for (numerator, denominator) in values {
            value.extend_from_slice(&encode_i32(order, *numerator));
            value.extend_from_slice(&encode_i32(order, *denominator));
        }
        TestEntry {
            tag,
            field_type: TYPE_SRATIONAL,
            count: values.len() as u32,
            value,
        }
    }

    fn identity_matrix_entry(order: DcpByteOrder, tag: u16) -> TestEntry {
        matrix_entry(
            order,
            tag,
            &[
                (1, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (1, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (1, 1),
            ],
        )
    }

    fn minimal_entries(order: DcpByteOrder) -> Vec<TestEntry> {
        vec![
            ascii_entry(TAG_UNIQUE_CAMERA_MODEL, TYPE_ASCII, b"Fixture Camera\0"),
            identity_matrix_entry(order, TAG_COLOR_MATRIX_1),
            short_entry(order, TAG_CALIBRATION_ILLUMINANT_1, 21),
            ascii_entry(TAG_PROFILE_NAME, TYPE_BYTE, b"P\0"),
        ]
    }

    fn valid_table_values(bias: f32) -> Vec<f32> {
        vec![
            bias,
            1.0,
            1.0,
            bias + 1.0,
            1.1,
            0.9,
            bias + 2.0,
            0.95,
            1.0,
            bias + 3.0,
            1.2,
            1.1,
        ]
    }

    fn find_entry_at(bytes: &[u8], order: DcpByteOrder, tag: u16) -> usize {
        let ifd = order.read_u32(bytes, 4).unwrap() as usize;
        let count = order.read_u16(bytes, ifd).unwrap() as usize;
        (0..count)
            .map(|index| ifd + 2 + index * 12)
            .find(|at| order.read_u16(bytes, *at).unwrap() == tag)
            .expect("fixture tag")
    }

    #[test]
    fn parses_minimal_profile_in_little_and_big_endian() {
        for order in [DcpByteOrder::LittleEndian, DcpByteOrder::BigEndian] {
            let profile = parse(&build_dcp(order, minimal_entries(order))).unwrap();
            assert_eq!(profile.byte_order, order);
            assert_eq!(
                profile.unique_camera_model.as_deref(),
                Some("Fixture Camera")
            );
            assert_eq!(profile.profile_name.as_deref(), Some("P"));
            assert_eq!(profile.calibrations.len(), 1);
            assert_eq!(profile.calibrations[0].illuminant, 21);
            assert_eq!(
                profile.calibrations[0].color_matrix,
                [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            );
            assert_eq!(profile.embed_policy, DcpEmbedPolicy::AllowCopying);
            assert_eq!(profile.creative, DcpCreativeMetadata::default());
        }
    }

    #[test]
    fn parses_dual_calibration_technical_luts_and_creative_metadata() {
        let order = DcpByteOrder::LittleEndian;
        let mut entries = minimal_entries(order);
        entries.extend([
            matrix_entry(
                order,
                TAG_COLOR_MATRIX_2,
                &[
                    (2, 1),
                    (0, 1),
                    (0, 1),
                    (0, 1),
                    (3, 1),
                    (0, 1),
                    (0, 1),
                    (0, 1),
                    (4, 1),
                ],
            ),
            short_entry(order, TAG_CALIBRATION_ILLUMINANT_2, 17),
            identity_matrix_entry(order, TAG_FORWARD_MATRIX_1),
            matrix_entry(
                order,
                TAG_FORWARD_MATRIX_2,
                &[
                    (9, 10),
                    (1, 20),
                    (1, 20),
                    (1, 20),
                    (9, 10),
                    (1, 20),
                    (1, 20),
                    (1, 20),
                    (9, 10),
                ],
            ),
            longs_entry(order, TAG_HUE_SAT_MAP_DIMS, &[2, 2, 1]),
            floats_entry(order, TAG_HUE_SAT_MAP_DATA_1, &valid_table_values(0.0)),
            floats_entry(order, TAG_HUE_SAT_MAP_DATA_2, &valid_table_values(10.0)),
            long_entry(order, TAG_HUE_SAT_MAP_ENCODING, 1),
            floats_entry(
                order,
                TAG_PROFILE_TONE_CURVE,
                &[0.0, 0.0, 0.5, 0.45, 1.0, 1.0],
            ),
            long_entry(order, TAG_PROFILE_EMBED_POLICY, 3),
            ascii_entry(TAG_PROFILE_CALIBRATION_SIGNATURE, TYPE_ASCII, b"cal-v1\0"),
            ascii_entry(TAG_PROFILE_COPYRIGHT, TYPE_BYTE, "© fixture\0".as_bytes()),
            longs_entry(order, TAG_LOOK_TABLE_DIMS, &[2, 2, 1]),
            floats_entry(order, TAG_LOOK_TABLE_DATA, &valid_table_values(20.0)),
            long_entry(order, TAG_LOOK_TABLE_ENCODING, 0),
        ]);

        let profile = parse(&build_dcp(order, entries)).unwrap();
        assert_eq!(profile.calibrations.len(), 2);
        assert_eq!(profile.calibrations[1].illuminant, 17);
        assert_eq!(profile.calibrations[1].color_matrix[2][2], 4.0);
        assert_eq!(
            profile.calibrations[0]
                .hue_sat_map
                .as_ref()
                .unwrap()
                .encoding,
            DcpTableEncoding::Srgb
        );
        assert_eq!(
            profile.calibrations[1]
                .hue_sat_map
                .as_ref()
                .unwrap()
                .entries[3]
                .hue_shift_degrees,
            13.0
        );
        assert_eq!(profile.embed_policy, DcpEmbedPolicy::NoRestrictions);
        assert_eq!(
            profile.profile_calibration_signature.as_deref(),
            Some("cal-v1")
        );
        assert_eq!(profile.copyright.as_deref(), Some("© fixture"));
        assert_eq!(profile.creative.tone_curve.as_ref().unwrap().len(), 3);
        let look = profile.creative.look_table.as_ref().unwrap();
        assert_eq!(look.encoding, DcpTableEncoding::Linear);
        assert_eq!(look.entries[3].hue_shift_degrees, 23.0);
    }

    #[test]
    fn skips_unknown_tags_without_dereferencing_untrusted_shape() {
        let order = DcpByteOrder::LittleEndian;
        let mut entries = minimal_entries(order);
        entries.insert(
            0,
            TestEntry {
                tag: 65000,
                field_type: u16::MAX,
                count: u32::MAX,
                value: Vec::new(),
            },
        );
        assert!(parse(&build_dcp(order, entries)).is_ok());
    }

    #[test]
    fn rejects_bad_headers_truncated_ifd_and_ifd_chain() {
        assert_eq!(parse(b"II"), Err(DcpError::Truncated));

        let order = DcpByteOrder::LittleEndian;
        let mut bad_order = build_dcp(order, minimal_entries(order));
        bad_order[0..2].copy_from_slice(b"ZZ");
        assert_eq!(parse(&bad_order), Err(DcpError::InvalidByteOrder));

        let mut bad_magic = build_dcp(order, minimal_entries(order));
        write_u16(&mut bad_magic, 2, order, 42);
        assert_eq!(
            parse(&bad_magic),
            Err(DcpError::InvalidMagic { actual: 42 })
        );

        let mut bad_ifd = build_dcp(order, minimal_entries(order));
        write_u32(&mut bad_ifd, 4, order, u32::MAX);
        assert_eq!(
            parse(&bad_ifd),
            Err(DcpError::InvalidIfdOffset { offset: u32::MAX })
        );

        let mut short_ifd = vec![0u8; 10];
        short_ifd[0..2].copy_from_slice(b"II");
        write_u16(&mut short_ifd, 2, order, DCP_MAGIC);
        write_u32(&mut short_ifd, 4, order, 8);
        write_u16(&mut short_ifd, 8, order, 1);
        assert_eq!(parse(&short_ifd), Err(DcpError::Truncated));

        let mut chained = build_dcp(order, minimal_entries(order));
        let next_ifd_at = 8 + 2 + minimal_entries(order).len() * 12;
        write_u32(&mut chained, next_ifd_at, order, 44);
        assert_eq!(
            parse(&chained),
            Err(DcpError::MultipleIfdsUnsupported {
                next_ifd_offset: 44
            })
        );
    }

    #[test]
    fn bounds_ifd_counts_huge_value_counts_and_offsets() {
        let order = DcpByteOrder::LittleEndian;
        let mut too_many = vec![0u8; 10];
        too_many[0..2].copy_from_slice(b"II");
        write_u16(&mut too_many, 2, order, DCP_MAGIC);
        write_u32(&mut too_many, 4, order, 8);
        write_u16(&mut too_many, 8, order, (MAX_IFD_ENTRIES + 1) as u16);
        assert!(matches!(
            parse(&too_many),
            Err(DcpError::TooManyIfdEntries { .. })
        ));

        let mut entries = minimal_entries(order);
        entries.push(TestEntry {
            tag: TAG_PROFILE_COPYRIGHT,
            field_type: TYPE_ASCII,
            count: u32::MAX,
            value: Vec::new(),
        });
        assert!(matches!(
            parse(&build_dcp(order, entries)),
            Err(DcpError::ResourceLimit {
                tag: TAG_PROFILE_COPYRIGHT,
                ..
            })
        ));

        let mut outside = build_dcp(order, minimal_entries(order));
        let name_at = find_entry_at(&outside, order, TAG_UNIQUE_CAMERA_MODEL);
        write_u32(&mut outside, name_at + 8, order, u32::MAX);
        assert_eq!(
            parse(&outside),
            Err(DcpError::ValueOutOfBounds {
                tag: TAG_UNIQUE_CAMERA_MODEL
            })
        );
    }

    #[test]
    fn rejects_duplicate_known_tag_and_wrong_types_or_counts() {
        let order = DcpByteOrder::LittleEndian;
        let mut duplicate = minimal_entries(order);
        duplicate.push(short_entry(order, TAG_CALIBRATION_ILLUMINANT_1, 17));
        assert_eq!(
            parse(&build_dcp(order, duplicate)),
            Err(DcpError::DuplicateTag {
                tag: TAG_CALIBRATION_ILLUMINANT_1
            })
        );

        let wrong_type = vec![
            floats_entry(order, TAG_COLOR_MATRIX_1, &[1.0; 9]),
            short_entry(order, TAG_CALIBRATION_ILLUMINANT_1, 21),
        ];
        assert!(matches!(
            parse(&build_dcp(order, wrong_type)),
            Err(DcpError::InvalidType {
                tag: TAG_COLOR_MATRIX_1,
                ..
            })
        ));

        let wrong_count = vec![
            matrix_entry(
                order,
                TAG_COLOR_MATRIX_1,
                &[
                    (1, 1),
                    (0, 1),
                    (0, 1),
                    (0, 1),
                    (1, 1),
                    (0, 1),
                    (0, 1),
                    (0, 1),
                ],
            ),
            short_entry(order, TAG_CALIBRATION_ILLUMINANT_1, 21),
        ];
        assert!(matches!(
            parse(&build_dcp(order, wrong_count)),
            Err(DcpError::InvalidCount {
                tag: TAG_COLOR_MATRIX_1,
                ..
            })
        ));
    }

    #[test]
    fn rejects_zero_denominator_and_singular_color_or_forward_matrices() {
        let order = DcpByteOrder::BigEndian;
        let mut zero_denominator = minimal_entries(order);
        zero_denominator[1] = matrix_entry(
            order,
            TAG_COLOR_MATRIX_1,
            &[
                (1, 0),
                (0, 1),
                (0, 1),
                (0, 1),
                (1, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (1, 1),
            ],
        );
        assert_eq!(
            parse(&build_dcp(order, zero_denominator)),
            Err(DcpError::ZeroDenominator {
                tag: TAG_COLOR_MATRIX_1,
                index: 0
            })
        );

        let singular_values = [
            (1, 1),
            (2, 1),
            (3, 1),
            (1, 1),
            (2, 1),
            (3, 1),
            (0, 1),
            (0, 1),
            (1, 1),
        ];
        let singular_color = vec![
            matrix_entry(order, TAG_COLOR_MATRIX_1, &singular_values),
            short_entry(order, TAG_CALIBRATION_ILLUMINANT_1, 21),
        ];
        assert_eq!(
            parse(&build_dcp(order, singular_color)),
            Err(DcpError::SingularMatrix {
                tag: TAG_COLOR_MATRIX_1
            })
        );

        let mut singular_forward = minimal_entries(order);
        singular_forward.push(matrix_entry(order, TAG_FORWARD_MATRIX_1, &singular_values));
        assert_eq!(
            parse(&build_dcp(order, singular_forward)),
            Err(DcpError::SingularMatrix {
                tag: TAG_FORWARD_MATRIX_1
            })
        );
    }

    #[test]
    fn rejects_nonfinite_and_mismatched_lut_data() {
        let order = DcpByteOrder::LittleEndian;
        let mut nonfinite = minimal_entries(order);
        let mut values = valid_table_values(0.0);
        values[4] = f32::NAN;
        nonfinite.extend([
            longs_entry(order, TAG_HUE_SAT_MAP_DIMS, &[2, 2, 1]),
            floats_entry(order, TAG_HUE_SAT_MAP_DATA_1, &values),
        ]);
        assert_eq!(
            parse(&build_dcp(order, nonfinite)),
            Err(DcpError::NonFinite {
                tag: TAG_HUE_SAT_MAP_DATA_1,
                index: 4
            })
        );

        let mut mismatch = minimal_entries(order);
        mismatch.extend([
            longs_entry(order, TAG_HUE_SAT_MAP_DIMS, &[2, 2, 1]),
            floats_entry(order, TAG_HUE_SAT_MAP_DATA_1, &[0.0; 9]),
        ]);
        assert_eq!(
            parse(&build_dcp(order, mismatch)),
            Err(DcpError::LutCountMismatch {
                tag: TAG_HUE_SAT_MAP_DATA_1,
                expected: 12,
                actual: 9
            })
        );
    }

    #[test]
    fn rejects_invalid_or_excessive_lut_dimensions_and_zero_saturation_rows() {
        let order = DcpByteOrder::LittleEndian;
        let mut invalid_dims = minimal_entries(order);
        invalid_dims.extend([
            longs_entry(order, TAG_HUE_SAT_MAP_DIMS, &[2, 1, 1]),
            floats_entry(order, TAG_HUE_SAT_MAP_DATA_1, &[0.0; 6]),
        ]);
        assert_eq!(
            parse(&build_dcp(order, invalid_dims)),
            Err(DcpError::InvalidTableDimensions {
                tag: TAG_HUE_SAT_MAP_DIMS
            })
        );

        let mut excessive = minimal_entries(order);
        excessive.extend([
            longs_entry(order, TAG_HUE_SAT_MAP_DIMS, &[MAX_LUT_CELLS as u32, 2, 1]),
            floats_entry(order, TAG_HUE_SAT_MAP_DATA_1, &[]),
        ]);
        assert!(matches!(
            parse(&build_dcp(order, excessive)),
            Err(DcpError::ResourceLimit {
                tag: TAG_HUE_SAT_MAP_DIMS,
                ..
            })
        ));

        let mut bad_zero_saturation = minimal_entries(order);
        let mut values = valid_table_values(0.0);
        values[2] = 0.5;
        bad_zero_saturation.extend([
            longs_entry(order, TAG_HUE_SAT_MAP_DIMS, &[2, 2, 1]),
            floats_entry(order, TAG_HUE_SAT_MAP_DATA_1, &values),
        ]);
        assert_eq!(
            parse(&build_dcp(order, bad_zero_saturation)),
            Err(DcpError::InvalidZeroSaturationEntry {
                tag: TAG_HUE_SAT_MAP_DATA_1,
                index: 0
            })
        );
    }

    #[test]
    fn rejects_malformed_strings_tone_curves_encoding_and_policy() {
        let order = DcpByteOrder::LittleEndian;
        let mut unterminated = minimal_entries(order);
        unterminated.push(ascii_entry(TAG_PROFILE_COPYRIGHT, TYPE_ASCII, b"bad"));
        assert_eq!(
            parse(&build_dcp(order, unterminated)),
            Err(DcpError::MissingStringTerminator {
                tag: TAG_PROFILE_COPYRIGHT
            })
        );

        let mut invalid_utf8 = minimal_entries(order);
        invalid_utf8.push(ascii_entry(
            TAG_PROFILE_CALIBRATION_SIGNATURE,
            TYPE_BYTE,
            &[0xff, 0],
        ));
        assert_eq!(
            parse(&build_dcp(order, invalid_utf8)),
            Err(DcpError::InvalidUtf8 {
                tag: TAG_PROFILE_CALIBRATION_SIGNATURE
            })
        );

        let mut descending_curve = minimal_entries(order);
        descending_curve.push(floats_entry(
            order,
            TAG_PROFILE_TONE_CURVE,
            &[0.0, 0.0, 0.7, 0.8, 0.6, 1.0],
        ));
        assert_eq!(
            parse(&build_dcp(order, descending_curve)),
            Err(DcpError::InvalidToneCurve)
        );

        let mut nonfinite_curve = minimal_entries(order);
        nonfinite_curve.push(floats_entry(
            order,
            TAG_PROFILE_TONE_CURVE,
            &[0.0, 0.0, 1.0, f32::INFINITY],
        ));
        assert!(matches!(
            parse(&build_dcp(order, nonfinite_curve)),
            Err(DcpError::NonFinite {
                tag: TAG_PROFILE_TONE_CURVE,
                ..
            })
        ));

        let mut bad_encoding = minimal_entries(order);
        bad_encoding.extend([
            longs_entry(order, TAG_HUE_SAT_MAP_DIMS, &[2, 2, 1]),
            floats_entry(order, TAG_HUE_SAT_MAP_DATA_1, &valid_table_values(0.0)),
            long_entry(order, TAG_HUE_SAT_MAP_ENCODING, 2),
        ]);
        assert_eq!(
            parse(&build_dcp(order, bad_encoding)),
            Err(DcpError::InvalidEncoding {
                tag: TAG_HUE_SAT_MAP_ENCODING,
                value: 2
            })
        );

        let mut bad_policy = minimal_entries(order);
        bad_policy.push(long_entry(order, TAG_PROFILE_EMBED_POLICY, 4));
        assert_eq!(
            parse(&build_dcp(order, bad_policy)),
            Err(DcpError::InvalidEmbedPolicy { value: 4 })
        );
    }

    #[test]
    fn rejects_incomplete_dual_and_orphaned_table_semantics() {
        let order = DcpByteOrder::LittleEndian;
        let mut incomplete = minimal_entries(order);
        incomplete.push(short_entry(order, TAG_CALIBRATION_ILLUMINANT_2, 17));
        assert_eq!(
            parse(&build_dcp(order, incomplete)),
            Err(DcpError::IncompleteSecondCalibration)
        );

        let mut mismatched_forward = minimal_entries(order);
        mismatched_forward.extend([
            identity_matrix_entry(order, TAG_COLOR_MATRIX_2),
            short_entry(order, TAG_CALIBRATION_ILLUMINANT_2, 17),
            identity_matrix_entry(order, TAG_FORWARD_MATRIX_1),
        ]);
        assert_eq!(
            parse(&build_dcp(order, mismatched_forward)),
            Err(DcpError::IncompleteSecondCalibration)
        );

        let mut orphaned_dims = minimal_entries(order);
        orphaned_dims.push(longs_entry(order, TAG_HUE_SAT_MAP_DIMS, &[2, 2, 1]));
        assert_eq!(
            parse(&build_dcp(order, orphaned_dims)),
            Err(DcpError::OrphanedTag {
                tag: TAG_HUE_SAT_MAP_DIMS
            })
        );

        let mut data_two_without_one = minimal_entries(order);
        data_two_without_one.extend([
            identity_matrix_entry(order, TAG_COLOR_MATRIX_2),
            short_entry(order, TAG_CALIBRATION_ILLUMINANT_2, 17),
            longs_entry(order, TAG_HUE_SAT_MAP_DIMS, &[2, 2, 1]),
            floats_entry(order, TAG_HUE_SAT_MAP_DATA_2, &valid_table_values(0.0)),
        ]);
        assert_eq!(
            parse(&build_dcp(order, data_two_without_one)),
            Err(DcpError::OrphanedTag {
                tag: TAG_HUE_SAT_MAP_DATA_2
            })
        );
    }

    #[test]
    fn rejects_custom_and_third_calibration_semantics_explicitly() {
        let order = DcpByteOrder::LittleEndian;
        let mut custom = minimal_entries(order);
        custom[2] = short_entry(order, TAG_CALIBRATION_ILLUMINANT_1, 255);
        assert_eq!(
            parse(&build_dcp(order, custom)),
            Err(DcpError::UnsupportedCustomIlluminant { calibration: 1 })
        );

        let mut custom_payload = minimal_entries(order);
        custom_payload.push(TestEntry {
            tag: TAG_ILLUMINANT_DATA_1,
            field_type: 7,
            count: 1,
            value: vec![0],
        });
        assert_eq!(
            parse(&build_dcp(order, custom_payload)),
            Err(DcpError::UnsupportedCustomIlluminant { calibration: 1 })
        );

        for third_tag in [
            TAG_CALIBRATION_ILLUMINANT_3,
            TAG_CAMERA_CALIBRATION_3,
            TAG_COLOR_MATRIX_3,
            TAG_FORWARD_MATRIX_3,
            TAG_ILLUMINANT_DATA_3,
            TAG_HUE_SAT_MAP_DATA_3,
            TAG_REDUCTION_MATRIX_3,
        ] {
            let mut third = minimal_entries(order);
            third.push(TestEntry {
                tag: third_tag,
                field_type: TYPE_BYTE,
                count: 1,
                value: vec![0],
            });
            assert_eq!(
                parse(&build_dcp(order, third)),
                Err(DcpError::UnsupportedThirdCalibration { tag: third_tag })
            );
        }
    }
}
