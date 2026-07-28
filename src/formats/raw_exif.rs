//! Minimal EXIF reader for TIFF-based RAW files (D4).
//!
//! The Develop window's EXIF line parses camera make/model plus the standard
//! shooting and lens tags straight out of the file's TIFF structure. IFD0 is
//! walked first, then the Exif sub-IFD it points to (0x8769). Non-TIFF
//! containers (CR3, RAF) simply return `None` and the UI hides the line.
//!
//! The parser is fully bounds-checked (it runs on worker bytes of arbitrary
//! files) and reads at most a few KB of IFD entries.

/// Shooting metadata for the Develop EXIF line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawExif {
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    /// Minimum/maximum focal length and minimum f-number at each end.
    pub lens_specification: Option<[f32; 4]>,
    pub iso: Option<u32>,
    pub f_number: Option<f32>,
    /// Exposure time in seconds.
    pub shutter_s: Option<f32>,
    /// Focal length in millimetres.
    pub focal_mm: Option<f32>,
}

impl RawExif {
    fn any(&self) -> bool {
        self.camera_make.is_some()
            || self.camera_model.is_some()
            || self.lens_make.is_some()
            || self.lens_model.is_some()
            || self.lens_specification.is_some()
            || self.iso.is_some()
            || self.f_number.is_some()
            || self.shutter_s.is_some()
            || self.focal_mm.is_some()
    }

    /// Camera, lens and exposure fields separated by middle dots; only fields
    /// that parsed are included.
    pub fn summary(&self) -> String {
        let mut equipment: Vec<String> = Vec::new();
        let mut exposure: Vec<String> = Vec::new();
        if let Some(camera) =
            joined_make_model(self.camera_make.as_deref(), self.camera_model.as_deref())
        {
            equipment.push(camera);
        }
        if let Some(lens) = joined_make_model(self.lens_make.as_deref(), self.lens_model.as_deref())
        {
            equipment.push(lens);
        } else if let Some(spec) = self.lens_specification {
            if let Some(lens) = format_lens_specification(spec) {
                equipment.push(lens);
            }
        }
        if let Some(iso) = self.iso {
            exposure.push(format!("ISO {iso}"));
        }
        if let Some(f) = self.f_number {
            if f > 0.0 {
                let s = format!("{f:.1}");
                exposure.push(format!("f/{}", s.trim_end_matches(".0")));
            }
        }
        if let Some(t) = self.shutter_s {
            if t > 0.0 {
                if t < 0.5 {
                    exposure.push(format!("1/{}s", (1.0 / t).round() as u32));
                } else {
                    let s = format!("{t:.1}");
                    exposure.push(format!("{}s", s.trim_end_matches(".0")));
                }
            }
        }
        if let Some(mm) = self.focal_mm {
            if mm > 0.0 {
                exposure.push(format!("{}mm", mm.round() as u32));
            }
        }
        [equipment.join(" · "), exposure.join(" · ")]
            .into_iter()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn format_lens_specification([min_mm, max_mm, min_f, max_f]: [f32; 4]) -> Option<String> {
    if min_mm <= 0.0 || max_mm <= 0.0 {
        return None;
    }
    let focal = if (min_mm - max_mm).abs() < 0.05 {
        format!("{}mm", min_mm.round() as u32)
    } else {
        format!("{}–{}mm", min_mm.round() as u32, max_mm.round() as u32)
    };
    let aperture = if min_f <= 0.0 || max_f <= 0.0 {
        String::new()
    } else if (min_f - max_f).abs() < 0.05 {
        format!(" f/{}", trim_decimal(min_f))
    } else {
        format!(" f/{}–{}", trim_decimal(min_f), trim_decimal(max_f))
    };
    Some(format!("{focal}{aperture}"))
}

fn trim_decimal(value: f32) -> String {
    let value = format!("{value:.1}");
    value.trim_end_matches(".0").to_string()
}

fn joined_make_model(make: Option<&str>, model: Option<&str>) -> Option<String> {
    match (make, model) {
        (Some(make), Some(model)) if model.to_lowercase().contains(&make.to_lowercase()) => {
            Some(model.to_string())
        }
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (Some(make), None) => Some(make.to_string()),
        (None, Some(model)) => Some(model.to_string()),
        (None, None) => None,
    }
}

/// Parse the Develop metadata line from a RAW file's bytes. `None` when the
/// file is not a TIFF container or carries none of the supported tags.
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

/// Walk one IFD, filling supported tags; returns the Exif sub-IFD offset
/// (tag 0x8769) if present.
fn scan_ifd(data: &[u8], offset: usize, le: bool, exif: &mut RawExif) -> Option<usize> {
    let count = read_u16(data, offset, le)? as usize;
    // An IFD with thousands of entries is corrupt; typical RAWs have < 100.
    let count = count.min(512);
    let mut exif_ifd = None;
    for i in 0..count {
        let e = offset.checked_add(2 + i * 12)?;
        let tag = read_u16(data, e, le)?;
        match tag {
            0x010F => {
                if exif.camera_make.is_none() {
                    exif.camera_make = read_ascii(data, e, le);
                }
            }
            0x0110 => {
                if exif.camera_model.is_none() {
                    exif.camera_model = read_ascii(data, e, le);
                }
            }
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
            0xA432 => {
                if exif.lens_specification.is_none() {
                    exif.lens_specification = read_rational_array4(data, e, le);
                }
            }
            0xA433 => {
                if exif.lens_make.is_none() {
                    exif.lens_make = read_ascii(data, e, le);
                }
            }
            0xA434 => {
                if exif.lens_model.is_none() {
                    exif.lens_model = read_ascii(data, e, le);
                }
            }
            _ => {}
        }
    }
    exif_ifd
}

/// ASCII (2) entry, stored inline when it fits in four bytes or at an offset.
fn read_ascii(data: &[u8], entry: usize, le: bool) -> Option<String> {
    if read_u16(data, entry + 2, le)? != 2 {
        return None;
    }
    let count = read_u32(data, entry + 4, le)? as usize;
    if count == 0 {
        return None;
    }
    let at = if count <= 4 {
        entry.checked_add(8)?
    } else {
        read_u32(data, entry + 8, le)? as usize
    };
    let bytes = data.get(at..at.checked_add(count)?)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let value = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    (!value.is_empty()).then_some(value)
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

fn read_rational_array4(data: &[u8], entry: usize, le: bool) -> Option<[f32; 4]> {
    if read_u16(data, entry + 2, le)? != 5 || read_u32(data, entry + 4, le)? < 4 {
        return None;
    }
    let at = read_u32(data, entry + 8, le)? as usize;
    let mut values = [0.0; 4];
    for (i, value) in values.iter_mut().enumerate() {
        let item = at.checked_add(i * 8)?;
        let num = read_u32(data, item, le)? as f32;
        let den = read_u32(data, item + 4, le)? as f32;
        if den == 0.0 {
            return None;
        }
        *value = num / den;
    }
    Some(values)
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

    fn camera_lens_tiff() -> Vec<u8> {
        let mut d = vec![0u8; 320];
        d[0..2].copy_from_slice(b"II");
        d[2..4].copy_from_slice(&42u16.to_le_bytes());
        d[4..8].copy_from_slice(&8u32.to_le_bytes());

        // IFD0: Make, Model and Exif sub-IFD pointer.
        d[8..10].copy_from_slice(&3u16.to_le_bytes());
        let entries = [
            (0x010Fu16, 80u32, b"Canon\0".as_slice()),
            (0x0110u16, 88u32, b"Canon EOS R5\0".as_slice()),
        ];
        for (i, (tag, offset, value)) in entries.iter().enumerate() {
            let e = 10 + i * 12;
            d[e..e + 2].copy_from_slice(&tag.to_le_bytes());
            d[e + 2..e + 4].copy_from_slice(&2u16.to_le_bytes());
            d[e + 4..e + 8].copy_from_slice(&(value.len() as u32).to_le_bytes());
            d[e + 8..e + 12].copy_from_slice(&offset.to_le_bytes());
            let at = *offset as usize;
            d[at..at + value.len()].copy_from_slice(value);
        }
        let e = 34;
        d[e..e + 2].copy_from_slice(&0x8769u16.to_le_bytes());
        d[e + 2..e + 4].copy_from_slice(&4u16.to_le_bytes());
        d[e + 4..e + 8].copy_from_slice(&1u32.to_le_bytes());
        d[e + 8..e + 12].copy_from_slice(&120u32.to_le_bytes());

        // Exif IFD: standard LensMake and LensModel.
        d[120..122].copy_from_slice(&2u16.to_le_bytes());
        let lens_entries = [
            (0xA433u16, 180u32, b"Canon\0".as_slice()),
            (
                0xA434u16,
                188u32,
                b"Canon RF 24-70mm F2.8 L IS USM\0".as_slice(),
            ),
        ];
        for (i, (tag, offset, value)) in lens_entries.iter().enumerate() {
            let e = 122 + i * 12;
            d[e..e + 2].copy_from_slice(&tag.to_le_bytes());
            d[e + 2..e + 4].copy_from_slice(&2u16.to_le_bytes());
            d[e + 4..e + 8].copy_from_slice(&(value.len() as u32).to_le_bytes());
            d[e + 8..e + 12].copy_from_slice(&offset.to_le_bytes());
            let at = *offset as usize;
            d[at..at + value.len()].copy_from_slice(value);
        }
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
    fn parses_camera_and_standard_lens_tags_without_repeating_make() {
        let exif = parse(&camera_lens_tiff()).expect("parse");
        assert_eq!(exif.camera_make.as_deref(), Some("Canon"));
        assert_eq!(exif.camera_model.as_deref(), Some("Canon EOS R5"));
        assert_eq!(exif.lens_make.as_deref(), Some("Canon"));
        assert_eq!(
            exif.lens_model.as_deref(),
            Some("Canon RF 24-70mm F2.8 L IS USM")
        );
        assert_eq!(
            exif.summary(),
            "Canon EOS R5 · Canon RF 24-70mm F2.8 L IS USM"
        );
    }

    #[test]
    fn puts_equipment_and_exposure_on_separate_lines() {
        let exif = RawExif {
            camera_make: Some("NIKON CORPORATION".into()),
            camera_model: Some("NIKON D810".into()),
            iso: Some(200),
            f_number: Some(4.0),
            shutter_s: Some(1.0 / 200.0),
            focal_mm: Some(170.0),
            ..RawExif::default()
        };
        assert_eq!(
            exif.summary(),
            "NIKON CORPORATION NIKON D810\nISO 200 · f/4 · 1/200s · 170mm"
        );
    }

    #[test]
    fn reads_short_ascii_inline_and_joins_distinct_make_model() {
        let mut d = vec![0u8; 64];
        d[0..2].copy_from_slice(b"II");
        d[2..4].copy_from_slice(&42u16.to_le_bytes());
        d[4..8].copy_from_slice(&8u32.to_le_bytes());
        d[8..10].copy_from_slice(&2u16.to_le_bytes());
        for (i, (tag, value)) in [(0x010Fu16, b"Fuji"), (0x0110u16, b"X-T5")]
            .iter()
            .enumerate()
        {
            let e = 10 + i * 12;
            d[e..e + 2].copy_from_slice(&tag.to_le_bytes());
            d[e + 2..e + 4].copy_from_slice(&2u16.to_le_bytes());
            d[e + 4..e + 8].copy_from_slice(&4u32.to_le_bytes());
            d[e + 8..e + 12].copy_from_slice(*value);
        }
        assert_eq!(parse(&d).unwrap().summary(), "Fuji X-T5");
    }

    #[test]
    fn uses_standard_lens_specification_when_model_is_missing() {
        let mut d = vec![0u8; 96];
        d[0..2].copy_from_slice(b"II");
        d[2..4].copy_from_slice(&42u16.to_le_bytes());
        d[4..8].copy_from_slice(&8u32.to_le_bytes());
        d[8..10].copy_from_slice(&1u16.to_le_bytes());
        let e = 10;
        d[e..e + 2].copy_from_slice(&0xA432u16.to_le_bytes());
        d[e + 2..e + 4].copy_from_slice(&5u16.to_le_bytes());
        d[e + 4..e + 8].copy_from_slice(&4u32.to_le_bytes());
        d[e + 8..e + 12].copy_from_slice(&40u32.to_le_bytes());
        for (i, (num, den)) in [(24u32, 1u32), (70, 1), (28, 10), (28, 10)]
            .iter()
            .enumerate()
        {
            let at = 40 + i * 8;
            d[at..at + 4].copy_from_slice(&num.to_le_bytes());
            d[at + 4..at + 8].copy_from_slice(&den.to_le_bytes());
        }
        let exif = parse(&d).unwrap();
        assert_eq!(exif.lens_specification, Some([24.0, 70.0, 2.8, 2.8]));
        assert_eq!(exif.summary(), "24–70mm f/2.8");
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
