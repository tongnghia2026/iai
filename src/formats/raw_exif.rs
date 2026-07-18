//! Minimal EXIF reader for TIFF-based RAW files (D4).
//!
//! rawloader does not expose shooting metadata, so the Develop window's EXIF
//! line parses the four tags it shows straight out of the file's TIFF
//! structure: ISO (0x8827), f-number (0x829D), exposure time (0x829A) and
//! focal length (0x920A). IFD0 is walked first, then the Exif sub-IFD it
//! points to (0x8769). Non-TIFF containers (CR3, RAF) simply return `None`
//! and the UI hides the line.
//!
//! The parser is fully bounds-checked (it runs on worker bytes of arbitrary
//! files) and reads at most a few KB of IFD entries.

/// Shooting metadata for the Develop EXIF line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawExif {
    pub iso: Option<u32>,
    pub f_number: Option<f32>,
    /// Exposure time in seconds.
    pub shutter_s: Option<f32>,
    /// Focal length in millimetres.
    pub focal_mm: Option<f32>,
}

impl RawExif {
    fn any(&self) -> bool {
        self.iso.is_some()
            || self.f_number.is_some()
            || self.shutter_s.is_some()
            || self.focal_mm.is_some()
    }

    /// "ISO 200 · f/2.8 · 1/250s · 35mm" — only the tags that parsed.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(iso) = self.iso {
            parts.push(format!("ISO {iso}"));
        }
        if let Some(f) = self.f_number {
            if f > 0.0 {
                let s = format!("{f:.1}");
                parts.push(format!("f/{}", s.trim_end_matches(".0")));
            }
        }
        if let Some(t) = self.shutter_s {
            if t > 0.0 {
                if t < 0.5 {
                    parts.push(format!("1/{}s", (1.0 / t).round() as u32));
                } else {
                    let s = format!("{t:.1}");
                    parts.push(format!("{}s", s.trim_end_matches(".0")));
                }
            }
        }
        if let Some(mm) = self.focal_mm {
            if mm > 0.0 {
                parts.push(format!("{}mm", mm.round() as u32));
            }
        }
        parts.join(" · ")
    }
}

/// Parse the EXIF line's tags from a RAW file's bytes. `None` when the file is
/// not a TIFF container or carries none of the four tags.
pub fn exif_summary(data: &[u8]) -> Option<String> {
    let exif = parse(data)?;
    exif.any().then(|| exif.summary())
}

fn parse(data: &[u8]) -> Option<RawExif> {
    let le = match data.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    // Accept classic TIFF (42) plus the TIFF-layout Olympus ORF ("RO"/"RS")
    // and Panasonic RW2 (0x55) magics — their IFDs read identically.
    let magic = read_u16(data, 2, le)?;
    if !matches!(magic, 42 | 0x4F52 | 0x5352 | 0x55) {
        return None;
    }
    let ifd0 = read_u32(data, 4, le)? as usize;

    let mut exif = RawExif::default();
    let exif_ifd = scan_ifd(data, ifd0, le, &mut exif);
    if let Some(off) = exif_ifd {
        scan_ifd(data, off, le, &mut exif);
    }
    Some(exif)
}

/// Walk one IFD, filling any of the four tags found; returns the Exif sub-IFD
/// offset (tag 0x8769) if present.
fn scan_ifd(data: &[u8], offset: usize, le: bool, exif: &mut RawExif) -> Option<usize> {
    let count = read_u16(data, offset, le)? as usize;
    // An IFD with thousands of entries is corrupt; typical RAWs have < 100.
    let count = count.min(512);
    let mut exif_ifd = None;
    for i in 0..count {
        let e = offset.checked_add(2 + i * 12)?;
        let tag = read_u16(data, e, le)?;
        match tag {
            0x8769 => {
                exif_ifd = read_u32(data, e + 8, le).map(|v| v as usize);
            }
            0x8827 => {
                if exif.iso.is_none() {
                    exif.iso = read_scalar(data, e, le);
                }
            }
            0x829D => {
                if exif.f_number.is_none() {
                    exif.f_number = read_rational(data, e, le);
                }
            }
            0x829A => {
                if exif.shutter_s.is_none() {
                    exif.shutter_s = read_rational(data, e, le);
                }
            }
            0x920A => {
                if exif.focal_mm.is_none() {
                    exif.focal_mm = read_rational(data, e, le);
                }
            }
            _ => {}
        }
    }
    exif_ifd
}

/// SHORT (3) or LONG (4) entry value (first element).
fn read_scalar(data: &[u8], entry: usize, le: bool) -> Option<u32> {
    let ty = read_u16(data, entry + 2, le)?;
    match ty {
        3 => read_u16(data, entry + 8, le).map(|v| v as u32),
        4 => read_u32(data, entry + 8, le),
        _ => None,
    }
}

/// RATIONAL (5) entry: value slot holds the offset of a num/den u32 pair.
fn read_rational(data: &[u8], entry: usize, le: bool) -> Option<f32> {
    let ty = read_u16(data, entry + 2, le)?;
    if ty != 5 {
        return None;
    }
    let at = read_u32(data, entry + 8, le)? as usize;
    let num = read_u32(data, at, le)? as f32;
    let den = read_u32(data, at + 4, le)? as f32;
    (den != 0.0 && num.is_finite()).then(|| num / den)
}

fn read_u16(data: &[u8], at: usize, le: bool) -> Option<u16> {
    let b = data.get(at..at + 2)?;
    Some(if le {
        u16::from_le_bytes([b[0], b[1]])
    } else {
        u16::from_be_bytes([b[0], b[1]])
    })
}

fn read_u32(data: &[u8], at: usize, le: bool) -> Option<u32> {
    let b = data.get(at..at + 4)?;
    Some(if le {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a little-endian TIFF: IFD0 with an Exif pointer, the Exif IFD
    /// carrying all four tags (rationals stored after the IFDs).
    fn synthetic_tiff() -> Vec<u8> {
        let mut d = vec![0u8; 200];
        d[0..2].copy_from_slice(b"II");
        d[2..4].copy_from_slice(&42u16.to_le_bytes());
        d[4..8].copy_from_slice(&8u32.to_le_bytes()); // IFD0 at 8

        // IFD0: 1 entry (Exif pointer -> 32).
        d[8..10].copy_from_slice(&1u16.to_le_bytes());
        let e = 10;
        d[e..e + 2].copy_from_slice(&0x8769u16.to_le_bytes());
        d[e + 2..e + 4].copy_from_slice(&4u16.to_le_bytes());
        d[e + 4..e + 8].copy_from_slice(&1u32.to_le_bytes());
        d[e + 8..e + 12].copy_from_slice(&32u32.to_le_bytes());

        // Exif IFD at 32: 4 entries.
        d[32..34].copy_from_slice(&4u16.to_le_bytes());
        let mut e = 34;
        // ISO: SHORT 200
        d[e..e + 2].copy_from_slice(&0x8827u16.to_le_bytes());
        d[e + 2..e + 4].copy_from_slice(&3u16.to_le_bytes());
        d[e + 4..e + 8].copy_from_slice(&1u32.to_le_bytes());
        d[e + 8..e + 10].copy_from_slice(&200u16.to_le_bytes());
        e += 12;
        // FNumber: RATIONAL 28/10 at 120
        d[e..e + 2].copy_from_slice(&0x829Du16.to_le_bytes());
        d[e + 2..e + 4].copy_from_slice(&5u16.to_le_bytes());
        d[e + 4..e + 8].copy_from_slice(&1u32.to_le_bytes());
        d[e + 8..e + 12].copy_from_slice(&120u32.to_le_bytes());
        d[120..124].copy_from_slice(&28u32.to_le_bytes());
        d[124..128].copy_from_slice(&10u32.to_le_bytes());
        e += 12;
        // ExposureTime: RATIONAL 1/250 at 128
        d[e..e + 2].copy_from_slice(&0x829Au16.to_le_bytes());
        d[e + 2..e + 4].copy_from_slice(&5u16.to_le_bytes());
        d[e + 4..e + 8].copy_from_slice(&1u32.to_le_bytes());
        d[e + 8..e + 12].copy_from_slice(&128u32.to_le_bytes());
        d[128..132].copy_from_slice(&1u32.to_le_bytes());
        d[132..136].copy_from_slice(&250u32.to_le_bytes());
        e += 12;
        // FocalLength: RATIONAL 350/10 at 136
        d[e..e + 2].copy_from_slice(&0x920Au16.to_le_bytes());
        d[e + 2..e + 4].copy_from_slice(&5u16.to_le_bytes());
        d[e + 4..e + 8].copy_from_slice(&1u32.to_le_bytes());
        d[e + 8..e + 12].copy_from_slice(&136u32.to_le_bytes());
        d[136..140].copy_from_slice(&350u32.to_le_bytes());
        d[140..144].copy_from_slice(&10u32.to_le_bytes());

        d
    }

    #[test]
    fn parses_all_four_tags_from_exif_ifd() {
        let exif = parse(&synthetic_tiff()).expect("parse");
        assert_eq!(exif.iso, Some(200));
        assert_eq!(exif.f_number, Some(2.8));
        assert!((exif.shutter_s.unwrap() - 1.0 / 250.0).abs() < 1e-6);
        assert_eq!(exif.focal_mm, Some(35.0));
        assert_eq!(exif.summary(), "ISO 200 · f/2.8 · 1/250s · 35mm");
        assert_eq!(
            exif_summary(&synthetic_tiff()).as_deref(),
            Some("ISO 200 · f/2.8 · 1/250s · 35mm")
        );
    }

    #[test]
    fn non_tiff_and_truncated_input_return_none() {
        assert_eq!(exif_summary(b"\xFF\xD8\xFF\xE0 not tiff"), None);
        assert_eq!(exif_summary(b""), None);
        assert_eq!(exif_summary(b"II"), None);
        // Header claims an IFD beyond the buffer: must not panic.
        let mut d = synthetic_tiff();
        d[4..8].copy_from_slice(&9999u32.to_le_bytes());
        assert_eq!(exif_summary(&d), None);
        // Truncate mid-IFD: rationals fall off the end, scalars still parse.
        let d = synthetic_tiff();
        assert!(exif_summary(&d[..70]).is_some());
    }

    #[test]
    fn big_endian_layout_parses() {
        let mut d = vec![0u8; 64];
        d[0..2].copy_from_slice(b"MM");
        d[2..4].copy_from_slice(&42u16.to_be_bytes());
        d[4..8].copy_from_slice(&8u32.to_be_bytes());
        d[8..10].copy_from_slice(&1u16.to_be_bytes());
        let e = 10;
        d[e..e + 2].copy_from_slice(&0x8827u16.to_be_bytes());
        d[e + 2..e + 4].copy_from_slice(&3u16.to_be_bytes());
        d[e + 4..e + 8].copy_from_slice(&1u32.to_be_bytes());
        d[e + 8..e + 10].copy_from_slice(&64000u16.to_be_bytes());
        let exif = parse(&d).expect("parse");
        assert_eq!(exif.iso, Some(64000));
        assert_eq!(exif.summary(), "ISO 64000");
    }
}
