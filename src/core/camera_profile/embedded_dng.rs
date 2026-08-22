//! Bounded extraction of the technical camera profile embedded in a DNG.
//!
//! A DNG carries its calibration tags (ColorMatrix, ForwardMatrix, illuminants,
//! technical HueSatMap, …) directly in classic-TIFF IFD0 rather than as a nested
//! standalone `.dcp` blob. This module reads ONLY those known technical tags —
//! seeking to each payload rather than scanning the whole file — and re-emits a
//! minimal standalone DCP container that [`super::dcp::parse`] then validates.
//!
//! Security posture: every offset, count, and length is bounds-checked against
//! the real source size before any allocation, so a hostile count cannot drive
//! an unbounded read. Creative look/tone tags are intentionally not copied; only
//! scene-linear technical characterization is extracted. The whole DNG is never
//! handed to `dcp::parse` (it caps the blob and expects the `0x4352` magic).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const TIFF_MAGIC: u16 = 42;
const DCP_MAGIC: u16 = 0x4352;

/// Ceiling on entries scanned in the DNG's first IFD.
const MAX_IFD0_ENTRIES: usize = 512;
/// Ceiling on a single copied tag payload (covers a full technical HueSatMap).
const MAX_TAG_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
/// Ceiling on the re-serialized standalone DCP.
const MAX_REEMIT_BYTES: usize = 16 * 1024 * 1024;

const TYPE_BYTE: u16 = 1;
const TYPE_ASCII: u16 = 2;
const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_RATIONAL: u16 = 5;
const TYPE_SRATIONAL: u16 = 10;
const TYPE_FLOAT: u16 = 11;

const TAG_COLOR_MATRIX_1: u16 = 50721;

/// Technical (scene-characterization) profile tags copied out of a DNG IFD0.
/// ColorMatrix1 is mandatory downstream; the rest are copied when present.
/// Creative ProfileLookTable/ProfileToneCurve tags are deliberately excluded.
const TECHNICAL_TAGS: &[u16] = &[
    50708, // UniqueCameraModel
    TAG_COLOR_MATRIX_1,
    50722, // ColorMatrix2
    50778,
    50779, // CalibrationIlluminant1/2
    50932, // ProfileCalibrationSignature
    50936, // ProfileName
    50937, // ProfileHueSatMapDims
    50938,
    50939, // ProfileHueSatMapData1/2
    50941, // ProfileEmbedPolicy
    50942, // ProfileCopyright
    50964,
    50965, // ForwardMatrix1/2
    51107, // ProfileHueSatMapEncoding
];

/// Byte order declared by the DNG TIFF header; preserved into the re-emitted DCP
/// so copied payloads stay valid without byte swapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ByteOrder {
    Little,
    Big,
}

impl ByteOrder {
    fn read_u16(self, bytes: &[u8], at: usize) -> u16 {
        let raw = [bytes[at], bytes[at + 1]];
        match self {
            Self::Little => u16::from_le_bytes(raw),
            Self::Big => u16::from_be_bytes(raw),
        }
    }

    fn read_u32(self, bytes: &[u8], at: usize) -> u32 {
        let raw = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
        match self {
            Self::Little => u32::from_le_bytes(raw),
            Self::Big => u32::from_be_bytes(raw),
        }
    }

    fn write_u16(self, out: &mut [u8], at: usize, value: u16) {
        let raw = match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        };
        out[at..at + 2].copy_from_slice(&raw);
    }

    fn write_u32(self, out: &mut [u8], at: usize, value: u32) {
        let raw = match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        };
        out[at..at + 4].copy_from_slice(&raw);
    }

    fn marker(self) -> [u8; 2] {
        match self {
            Self::Little => *b"II",
            Self::Big => *b"MM",
        }
    }
}

/// A structural failure while extracting an embedded DNG profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EmbeddedDngError {
    Truncated,
    InvalidByteOrder,
    NotTiff {
        magic: u16,
    },
    InvalidIfdOffset {
        offset: u32,
    },
    TooManyEntries {
        actual: usize,
        maximum: usize,
    },
    DuplicateTag {
        tag: u16,
    },
    InvalidTagOffset {
        tag: u16,
    },
    PayloadTooLarge {
        tag: u16,
        actual: usize,
        maximum: usize,
    },
    ReemitTooLarge {
        actual: usize,
        maximum: usize,
    },
    Overflow,
    Io(std::io::ErrorKind),
}

impl std::fmt::Display for EmbeddedDngError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => f.write_str("DNG is truncated"),
            Self::InvalidByteOrder => f.write_str("DNG has no II/MM byte-order marker"),
            Self::NotTiff { magic } => write!(f, "DNG TIFF magic is {magic}, expected 42"),
            Self::InvalidIfdOffset { offset } => write!(f, "invalid IFD0 offset {offset}"),
            Self::TooManyEntries { actual, maximum } => {
                write!(f, "IFD0 has {actual} entries; maximum is {maximum}")
            }
            Self::DuplicateTag { tag } => write!(f, "duplicate technical tag {tag} in IFD0"),
            Self::InvalidTagOffset { tag } => write!(f, "tag {tag} payload is out of bounds"),
            Self::PayloadTooLarge {
                tag,
                actual,
                maximum,
            } => write!(
                f,
                "tag {tag} payload is {actual} bytes; maximum is {maximum}"
            ),
            Self::ReemitTooLarge { actual, maximum } => {
                write!(f, "re-emitted DCP is {actual} bytes; maximum is {maximum}")
            }
            Self::Overflow => f.write_str("offset arithmetic overflowed"),
            Self::Io(kind) => write!(f, "read error: {kind:?}"),
        }
    }
}

impl std::error::Error for EmbeddedDngError {}

/// A random-access byte source that only reads the small regions requested.
trait ByteSource {
    fn len(&mut self) -> Result<u64, EmbeddedDngError>;
    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, EmbeddedDngError>;
}

struct SliceSource<'a> {
    bytes: &'a [u8],
}

impl ByteSource for SliceSource<'_> {
    fn len(&mut self) -> Result<u64, EmbeddedDngError> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, EmbeddedDngError> {
        let start = usize::try_from(offset).map_err(|_| EmbeddedDngError::Overflow)?;
        let end = start.checked_add(len).ok_or(EmbeddedDngError::Overflow)?;
        let slice = self
            .bytes
            .get(start..end)
            .ok_or(EmbeddedDngError::Truncated)?;
        Ok(slice.to_vec())
    }
}

struct FileSource {
    file: File,
    len: u64,
}

impl ByteSource for FileSource {
    fn len(&mut self) -> Result<u64, EmbeddedDngError> {
        Ok(self.len)
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, EmbeddedDngError> {
        let end = offset
            .checked_add(len as u64)
            .ok_or(EmbeddedDngError::Overflow)?;
        if end > self.len {
            return Err(EmbeddedDngError::Truncated);
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|error| EmbeddedDngError::Io(error.kind()))?;
        let mut buffer = vec![0u8; len];
        self.file
            .read_exact(&mut buffer)
            .map_err(|error| EmbeddedDngError::Io(error.kind()))?;
        Ok(buffer)
    }
}

/// Bytes-per-element for a TIFF field type, or `None` for types this extractor
/// does not need to size.
fn type_size(field_type: u16) -> Option<usize> {
    match field_type {
        TYPE_BYTE | TYPE_ASCII => Some(1),
        TYPE_SHORT => Some(2),
        TYPE_LONG | TYPE_FLOAT => Some(4),
        TYPE_RATIONAL | TYPE_SRATIONAL => Some(8),
        _ => None,
    }
}

struct CopiedTag {
    tag: u16,
    field_type: u16,
    count: u32,
    value: TagValue,
}

enum TagValue {
    Inline([u8; 4]),
    Payload(Vec<u8>),
}

fn extract(source: &mut impl ByteSource) -> Result<Option<Vec<u8>>, EmbeddedDngError> {
    let total = source.len()?;
    if total < 8 {
        return Err(EmbeddedDngError::Truncated);
    }
    let header = source.read_at(0, 8)?;
    let order = match &header[0..2] {
        b"II" => ByteOrder::Little,
        b"MM" => ByteOrder::Big,
        _ => return Err(EmbeddedDngError::InvalidByteOrder),
    };
    let magic = order.read_u16(&header, 2);
    if magic != TIFF_MAGIC {
        return Err(EmbeddedDngError::NotTiff { magic });
    }
    let ifd0_u32 = order.read_u32(&header, 4);
    let ifd0 = u64::from(ifd0_u32);
    if ifd0 < 8 || ifd0 >= total {
        return Err(EmbeddedDngError::InvalidIfdOffset { offset: ifd0_u32 });
    }

    let count_bytes = source.read_at(ifd0, 2)?;
    let entry_count = usize::from(order.read_u16(&count_bytes, 0));
    if entry_count > MAX_IFD0_ENTRIES {
        return Err(EmbeddedDngError::TooManyEntries {
            actual: entry_count,
            maximum: MAX_IFD0_ENTRIES,
        });
    }
    if entry_count == 0 {
        return Ok(None);
    }
    let entries_start = ifd0.checked_add(2).ok_or(EmbeddedDngError::Overflow)?;
    let entries_len = entry_count
        .checked_mul(12)
        .ok_or(EmbeddedDngError::Overflow)?;
    let entries_end = entries_start
        .checked_add(entries_len as u64)
        .and_then(|end| end.checked_add(4))
        .ok_or(EmbeddedDngError::Overflow)?;
    if entries_end > total {
        return Err(EmbeddedDngError::Truncated);
    }
    let entries = source.read_at(entries_start, entries_len)?;

    let mut copied: Vec<CopiedTag> = Vec::new();
    for index in 0..entry_count {
        let base = index * 12;
        let tag = order.read_u16(&entries, base);
        if !TECHNICAL_TAGS.contains(&tag) {
            continue;
        }
        if copied.iter().any(|existing| existing.tag == tag) {
            return Err(EmbeddedDngError::DuplicateTag { tag });
        }
        let field_type = order.read_u16(&entries, base + 2);
        let count = order.read_u32(&entries, base + 4);
        let value_field: [u8; 4] = entries[base + 8..base + 12]
            .try_into()
            .map_err(|_| EmbeddedDngError::Overflow)?;

        let Some(size) = type_size(field_type) else {
            // Unknown/unsupported type on a technical tag: skip it. dcp::parse
            // then rejects the profile if a required tag went missing.
            continue;
        };
        let payload_len = (count as usize)
            .checked_mul(size)
            .ok_or(EmbeddedDngError::Overflow)?;
        if payload_len > MAX_TAG_PAYLOAD_BYTES {
            return Err(EmbeddedDngError::PayloadTooLarge {
                tag,
                actual: payload_len,
                maximum: MAX_TAG_PAYLOAD_BYTES,
            });
        }

        let value = if payload_len <= 4 {
            TagValue::Inline(value_field)
        } else {
            let offset = u64::from(order.read_u32(&value_field, 0));
            let end = offset
                .checked_add(payload_len as u64)
                .ok_or(EmbeddedDngError::Overflow)?;
            if offset < 8 || end > total {
                return Err(EmbeddedDngError::InvalidTagOffset { tag });
            }
            TagValue::Payload(source.read_at(offset, payload_len)?)
        };
        copied.push(CopiedTag {
            tag,
            field_type,
            count,
            value,
        });
    }

    // A technical profile needs at least ColorMatrix1; without it there is
    // nothing to characterize and dcp::parse would reject the re-emission.
    if !copied.iter().any(|entry| entry.tag == TAG_COLOR_MATRIX_1) {
        return Ok(None);
    }
    copied.sort_by_key(|entry| entry.tag);
    Ok(Some(reemit(order, &copied)?))
}

/// Emit a standalone single-IFD DCP (magic `0x4352`) holding the copied tags,
/// in the source byte order so copied payloads stay valid without swapping.
fn reemit(order: ByteOrder, copied: &[CopiedTag]) -> Result<Vec<u8>, EmbeddedDngError> {
    let entry_count = copied.len();
    let ifd_at = 8usize;
    let payload_start = ifd_at
        .checked_add(2)
        .and_then(|value| value.checked_add(entry_count.checked_mul(12)?))
        .and_then(|value| value.checked_add(4))
        .ok_or(EmbeddedDngError::Overflow)?;

    let mut out = vec![0u8; payload_start];
    out[0..2].copy_from_slice(&order.marker());
    order.write_u16(&mut out, 2, DCP_MAGIC);
    order.write_u32(&mut out, 4, ifd_at as u32);
    order.write_u16(&mut out, ifd_at, entry_count as u16);

    let mut payload: Vec<u8> = Vec::new();
    for (index, entry) in copied.iter().enumerate() {
        let at = ifd_at + 2 + index * 12;
        order.write_u16(&mut out, at, entry.tag);
        order.write_u16(&mut out, at + 2, entry.field_type);
        order.write_u32(&mut out, at + 4, entry.count);
        match &entry.value {
            TagValue::Inline(bytes) => out[at + 8..at + 12].copy_from_slice(bytes),
            TagValue::Payload(data) => {
                let offset = payload_start
                    .checked_add(payload.len())
                    .ok_or(EmbeddedDngError::Overflow)?;
                if offset > u32::MAX as usize {
                    return Err(EmbeddedDngError::Overflow);
                }
                order.write_u32(&mut out, at + 8, offset as u32);
                payload.extend_from_slice(data);
                if payload.len() % 2 != 0 {
                    payload.push(0);
                }
            }
        }
        let projected = payload_start
            .checked_add(payload.len())
            .ok_or(EmbeddedDngError::Overflow)?;
        if projected > MAX_REEMIT_BYTES {
            return Err(EmbeddedDngError::ReemitTooLarge {
                actual: projected,
                maximum: MAX_REEMIT_BYTES,
            });
        }
    }
    // The 4-byte next-IFD pointer at `payload_start - 4` stays zero.
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Extract a re-serialized standalone technical DCP from DNG bytes already in
/// memory. `Ok(None)` means the file is a valid TIFF but carries no technical
/// camera profile (no ColorMatrix1).
pub fn extract_technical_dcp(bytes: &[u8]) -> Result<Option<Vec<u8>>, EmbeddedDngError> {
    extract(&mut SliceSource { bytes })
}

/// Extract a re-serialized standalone technical DCP from a DNG file, seeking to
/// only the header, IFD0, and the technical payloads rather than reading the
/// whole (often very large) file.
pub fn extract_technical_dcp_from_file(path: &Path) -> Result<Option<Vec<u8>>, EmbeddedDngError> {
    let file = File::open(path).map_err(|error| EmbeddedDngError::Io(error.kind()))?;
    let len = file
        .metadata()
        .map_err(|error| EmbeddedDngError::Io(error.kind()))?
        .len();
    extract(&mut FileSource { file, len })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::camera_profile::dcp;

    struct Entry {
        tag: u16,
        field_type: u16,
        count: u32,
        bytes: Vec<u8>,
    }

    fn ascii(tag: u16, text: &str) -> Entry {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        Entry {
            tag,
            field_type: TYPE_ASCII,
            count: bytes.len() as u32,
            bytes,
        }
    }

    fn srational_matrix(tag: u16, values: [i32; 9]) -> Entry {
        let mut bytes = Vec::with_capacity(72);
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
            bytes.extend_from_slice(&1i32.to_le_bytes());
        }
        Entry {
            tag,
            field_type: TYPE_SRATIONAL,
            count: 9,
            bytes,
        }
    }

    fn short(tag: u16, value: u16) -> Entry {
        Entry {
            tag,
            field_type: TYPE_SHORT,
            count: 1,
            bytes: value.to_le_bytes().to_vec(),
        }
    }

    /// Build a little-endian TIFF/DNG with the given IFD0 entries plus one
    /// non-profile noise tag that must be ignored.
    fn build_dng(entries: Vec<Entry>) -> Vec<u8> {
        let ifd_at = 8usize;
        let entry_count = entries.len();
        let data_at = ifd_at + 2 + entry_count * 12 + 4;
        let mut out = vec![0u8; data_at];
        out[0..2].copy_from_slice(b"II");
        out[2..4].copy_from_slice(&TIFF_MAGIC.to_le_bytes());
        out[4..8].copy_from_slice(&(ifd_at as u32).to_le_bytes());
        out[ifd_at..ifd_at + 2].copy_from_slice(&(entry_count as u16).to_le_bytes());

        let mut payload = Vec::new();
        for (index, entry) in entries.into_iter().enumerate() {
            let at = ifd_at + 2 + index * 12;
            out[at..at + 2].copy_from_slice(&entry.tag.to_le_bytes());
            out[at + 2..at + 4].copy_from_slice(&entry.field_type.to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&entry.count.to_le_bytes());
            if entry.bytes.len() <= 4 {
                out[at + 8..at + 8 + entry.bytes.len()].copy_from_slice(&entry.bytes);
            } else {
                let offset = (data_at + payload.len()) as u32;
                out[at + 8..at + 12].copy_from_slice(&offset.to_le_bytes());
                payload.extend_from_slice(&entry.bytes);
                if payload.len() % 2 != 0 {
                    payload.push(0);
                }
            }
        }
        out.extend_from_slice(&payload);
        out
    }

    fn full_profile_entries() -> Vec<Entry> {
        vec![
            // A non-profile noise tag (ImageWidth) that must be ignored.
            short(256, 4272),
            ascii(50708, "Canon EOS 550D"),
            srational_matrix(50721, [1, 0, 0, 0, 1, 0, 0, 0, 1]),
            short(50778, 21), // CalibrationIlluminant1 = D65
            ascii(50936, "Embedded"),
        ]
    }

    #[test]
    fn extracts_and_reparses_technical_profile() {
        let dng = build_dng(full_profile_entries());
        let blob = extract_technical_dcp(&dng)
            .expect("extraction succeeds")
            .expect("a technical profile is present");
        // The re-emitted blob is a standalone DCP the tested parser accepts.
        let profile = dcp::parse(&blob).expect("re-emitted DCP parses");
        assert_eq!(
            profile.unique_camera_model.as_deref(),
            Some("Canon EOS 550D")
        );
        assert_eq!(profile.profile_name.as_deref(), Some("Embedded"));
        assert_eq!(profile.calibrations.len(), 1);
        assert_eq!(profile.calibrations[0].illuminant, 21);
        assert_eq!(
            profile.calibrations[0].color_matrix,
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        );
    }

    #[test]
    fn big_endian_dng_round_trips() {
        // Re-encode the same profile as big-endian by swapping the header and
        // multi-byte fields; the extractor must preserve order end to end.
        let little = build_dng(full_profile_entries());
        let big = to_big_endian(&little);
        let blob = extract_technical_dcp(&big)
            .expect("big-endian extraction succeeds")
            .expect("profile present");
        assert_eq!(&blob[0..2], b"MM");
        let profile = dcp::parse(&blob).expect("re-emitted big-endian DCP parses");
        assert_eq!(
            profile.unique_camera_model.as_deref(),
            Some("Canon EOS 550D")
        );
        assert_eq!(profile.calibrations[0].illuminant, 21);
    }

    #[test]
    fn no_color_matrix_yields_none() {
        let entries = vec![ascii(50708, "Canon EOS 550D"), short(50778, 21)];
        let dng = build_dng(entries);
        assert_eq!(extract_technical_dcp(&dng), Ok(None));
    }

    #[test]
    fn rejects_non_tiff_and_truncated() {
        assert_eq!(
            extract_technical_dcp(b"not a tiff at all"),
            Err(EmbeddedDngError::InvalidByteOrder)
        );
        assert_eq!(
            extract_technical_dcp(&[0u8; 4]),
            Err(EmbeddedDngError::Truncated)
        );
        let mut wrong_magic = build_dng(full_profile_entries());
        wrong_magic[2..4].copy_from_slice(&7u16.to_le_bytes());
        assert_eq!(
            extract_technical_dcp(&wrong_magic),
            Err(EmbeddedDngError::NotTiff { magic: 7 })
        );
    }

    #[test]
    fn rejects_hostile_tag_offset() {
        let mut dng = build_dng(full_profile_entries());
        // Point ColorMatrix1 (a payload tag) at an offset far past the file.
        let ifd_at = 8usize;
        let entry_count = u16::from_le_bytes([dng[ifd_at], dng[ifd_at + 1]]) as usize;
        for index in 0..entry_count {
            let at = ifd_at + 2 + index * 12;
            let tag = u16::from_le_bytes([dng[at], dng[at + 1]]);
            if tag == TAG_COLOR_MATRIX_1 {
                dng[at + 8..at + 12].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
            }
        }
        assert_eq!(
            extract_technical_dcp(&dng),
            Err(EmbeddedDngError::InvalidTagOffset {
                tag: TAG_COLOR_MATRIX_1
            })
        );
    }

    #[test]
    fn rejects_too_many_entries() {
        let mut dng = build_dng(full_profile_entries());
        // Overstate the IFD0 entry count without growing the file.
        let ifd_at = 8usize;
        dng[ifd_at..ifd_at + 2].copy_from_slice(&((MAX_IFD0_ENTRIES + 1) as u16).to_le_bytes());
        assert_eq!(
            extract_technical_dcp(&dng),
            Err(EmbeddedDngError::TooManyEntries {
                actual: MAX_IFD0_ENTRIES + 1,
                maximum: MAX_IFD0_ENTRIES,
            })
        );
    }

    /// Convert a little-endian DNG fixture (from `build_dng`) to big-endian:
    /// swap the header word order, every IFD entry's tag/type/count/offset, and
    /// every SRATIONAL/SHORT payload element. Only used to prove order handling.
    fn to_big_endian(little: &[u8]) -> Vec<u8> {
        let mut out = little.to_vec();
        out[0..2].copy_from_slice(b"MM");
        swap_u16(&mut out, 2);
        swap_u32(&mut out, 4);
        let ifd_at = 8usize;
        let entry_count = u16::from_le_bytes([out[ifd_at], out[ifd_at + 1]]) as usize;
        swap_u16(&mut out, ifd_at);
        for index in 0..entry_count {
            let at = ifd_at + 2 + index * 12;
            let field_type = u16::from_le_bytes([out[at + 2], out[at + 3]]);
            let count = u32::from_le_bytes([out[at + 4], out[at + 5], out[at + 6], out[at + 7]]);
            let size = type_size(field_type).unwrap();
            let payload_len = count as usize * size;
            let offset = if payload_len > 4 {
                Some(
                    u32::from_le_bytes([out[at + 8], out[at + 9], out[at + 10], out[at + 11]])
                        as usize,
                )
            } else {
                None
            };
            swap_u16(&mut out, at); // tag
            swap_u16(&mut out, at + 2); // type
            swap_u32(&mut out, at + 4); // count
            match (offset, field_type) {
                (Some(offset), TYPE_SRATIONAL | TYPE_RATIONAL) => {
                    for element in 0..count as usize {
                        swap_u32(&mut out, offset + element * 8);
                        swap_u32(&mut out, offset + element * 8 + 4);
                    }
                    swap_u32(&mut out, at + 8); // offset field
                }
                (Some(offset), _) => {
                    swap_u32(&mut out, at + 8);
                    let _ = offset;
                }
                (None, TYPE_SHORT) => swap_u16(&mut out, at + 8),
                (None, _) => {}
            }
        }
        out
    }

    fn swap_u16(bytes: &mut [u8], at: usize) {
        let value = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        bytes[at..at + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn swap_u32(bytes: &mut [u8], at: usize) {
        let value = u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }
}
