//! The library that decodes camera RAW files for iAi.
//!
//! rawloader reads the file; what rawloader does not read goes to rawler,
//! which adds Canon CR3 and a much larger camera database. Either way the
//! answer is the app's own [`RawImage`] (`iai-raw-api`), so that nothing of a
//! decoder reaches the app.
//!
//! The app has this crate one of two ways: linked in, calling [`decode_file`]
//! and [`orientation`]; or as a file of its own beside the exe
//! (`iai_raw.dll`), calling the C functions of [`exports`].

use std::path::Path;

pub use iai_raw_api::{Cfa, Decoder, Orientation, RawImage, RawImageData};

pub mod exports;

/// Decode the RAW file at `path`. Files rawloader decodes never reach rawler.
pub fn decode_file(path: &Path) -> Result<RawImage, String> {
    guarded(|| match rawloader::decode_file(path) {
        Ok(image) => to_app(image, Decoder::Rawloader),
        Err(primary) => decode_via_rawler(path)
            .and_then(|image| to_app(image, Decoder::Rawler))
            .map_err(|fallback| format!("rawloader: {primary}; {fallback}")),
    })
    .unwrap_or_else(|panic| Err(format!("the RAW decoder gave up on this file: {panic}")))
}

/// The EXIF orientation of the RAW file whose bytes `file` is, read without
/// decoding the picture. Any failure is [`Orientation::Unknown`].
pub fn orientation(file: &[u8]) -> Orientation {
    guarded(|| {
        rawloader::decode_dummy(&mut std::io::Cursor::new(file))
            .map(|image| Orientation::from_exif(image.orientation.to_u16()))
            .unwrap_or(Orientation::Unknown)
    })
    .unwrap_or(Orientation::Unknown)
}

/// Run a decoder, turning a panic of its into what it said. The decoders
/// panic on some malformed files.
fn guarded<T>(decode: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode)).map_err(|payload| {
        if let Some(text) = payload.downcast_ref::<&str>() {
            (*text).to_owned()
        } else if let Some(text) = payload.downcast_ref::<String>() {
            text.clone()
        } else {
            "panic".to_owned()
        }
    })
}

fn cfa_of(cfa: &rawloader::CFA) -> Result<Cfa, String> {
    let mut colors = Vec::with_capacity(cfa.width * cfa.height);
    for row in 0..cfa.height {
        for col in 0..cfa.width {
            colors.push(cfa.color_at(row, col) as u8);
        }
    }
    Cfa::new(&cfa.name, cfa.width, cfa.height, &colors)
}

/// rawloader's picture as the app takes it. The two values the app used to
/// ask rawloader for afterwards are worked out here, by rawloader.
fn to_app(image: rawloader::RawImage, decoder: Decoder) -> Result<RawImage, String> {
    let neutral_wb = image.neutralwb();
    let cam_to_xyz_normalized = image.cam_to_xyz_normalized();
    let cfa = cfa_of(&image.cfa)?;
    Ok(RawImage {
        decoder,
        make: image.make,
        model: image.model,
        clean_make: image.clean_make,
        clean_model: image.clean_model,
        width: image.width,
        height: image.height,
        cpp: image.cpp,
        wb_coeffs: image.wb_coeffs,
        neutral_wb,
        whitelevels: image.whitelevels,
        blacklevels: image.blacklevels,
        xyz_to_cam: image.xyz_to_cam,
        cam_to_xyz_normalized,
        cfa,
        crops: image.crops,
        blackareas: image
            .blackareas
            .into_iter()
            .map(|(top, right, bottom, left)| [top, right, bottom, left])
            .collect(),
        orientation: Orientation::from_exif(image.orientation.to_u16()),
        data: match image.data {
            rawloader::RawImageData::Integer(samples) => RawImageData::Integer(samples.into()),
            rawloader::RawImageData::Float(samples) => RawImageData::Float(samples.into()),
        },
    })
}

/// Decode a RAW with rawler and adapt it into a `rawloader::RawImage`, so that
/// both decoders leave through [`to_app`]. rawler's public data model maps
/// onto rawloader's one-to-one: same mosaic enum shape, a `[[f32; 3]; 4]`
/// `xyz_to_cam`, an EXIF-numbered orientation, and a CFA whose `name` is the
/// pattern string rawloader's `CFA::new` expects.
fn decode_via_rawler(path: &Path) -> Result<rawloader::RawImage, String> {
    let img = rawler::decode_file(path).map_err(|e| format!("rawler: {e:?}"))?;

    // rawler's camera database stores characterization in `color_matrix`;
    // `xyz_to_cam` is deprecated and is commonly left as an all-zero default.
    // Select a matrix deterministically before adapting into rawloader's data
    // model, otherwise supported rawler-only cameras can render black.
    let mono = matches!(
        img.photometric,
        rawler::rawimage::RawPhotometricInterpretation::BlackIsZero
    );
    let xyz_to_cam = select_rawler_xyz_to_cam(&img.color_matrix, img.xyz_to_cam, mono)?;

    let to_u16_4 = |a: [f32; 4]| a.map(|v| v.round().clamp(0.0, u16::MAX as f32) as u16);
    let whitelevels = to_u16_4(img.whitelevel.as_bayer_array());
    let blacklevels = to_u16_4(img.blacklevel.as_bayer_array());
    // Preserve rawler's masked optical-black rectangles across the compatibility
    // adapter. The pixel path already uses rawler's resolved black levels; this
    // only keeps the provenance/audit data that the old adapter discarded.
    let blackareas = img
        .blackareas
        .iter()
        .map(|rect| {
            (
                rect.p.y as u64,
                rect.p.x.saturating_add(rect.d.w) as u64,
                rect.p.y.saturating_add(rect.d.h) as u64,
                rect.p.x as u64,
            )
        })
        .collect();

    // rawloader crops are margins [top, right, bottom, left]. Prefer the default
    // crop, then the active area, else the whole frame.
    let crops = img
        .crop_area
        .or(img.active_area)
        .map(|r| {
            let [top, left, bottom, right] = r.as_tlbr_offsets(img.width, img.height);
            [top, right, bottom, left]
        })
        .unwrap_or([0, 0, 0, 0]);

    let cfa = rawloader::CFA::new(&img.camera.cfa.name);
    let orientation = rawloader::Orientation::from_u16(img.orientation.to_u16());
    let data = match img.data {
        rawler::rawimage::RawImageData::Integer(v) => rawloader::RawImageData::Integer(v),
        rawler::rawimage::RawImageData::Float(v) => rawloader::RawImageData::Float(v),
    };

    Ok(rawloader::RawImage {
        make: img.make,
        model: img.model,
        clean_make: img.clean_make,
        clean_model: img.clean_model,
        width: img.width,
        height: img.height,
        cpp: img.cpp,
        wb_coeffs: img.wb_coeffs,
        whitelevels,
        blacklevels,
        xyz_to_cam,
        cfa,
        crops,
        orientation,
        data,
        blackareas,
    })
}

fn select_rawler_xyz_to_cam(
    color_matrices: &std::collections::HashMap<
        rawler::imgop::xyz::Illuminant,
        rawler::imgop::xyz::FlatColorMatrix,
    >,
    legacy: [[f32; 3]; 4],
    mono: bool,
) -> Result<[[f32; 3]; 4], String> {
    use rawler::imgop::xyz::Illuminant;

    fn usable_flat(matrix: &[f32]) -> bool {
        matches!(matrix.len(), 9 | 12)
            && matrix.iter().all(|value| value.is_finite())
            && matrix
                .chunks_exact(3)
                .all(|row| row.iter().map(|value| value.abs()).sum::<f32>() > 1.0e-8)
    }

    fn usable_legacy(matrix: &[[f32; 3]; 4]) -> bool {
        matrix.iter().flatten().all(|value| value.is_finite())
            && matrix[..3]
                .iter()
                .all(|row| row.iter().map(|value| value.abs()).sum::<f32>() > 1.0e-8)
    }

    fn rank(illuminant: Illuminant) -> (u8, u16) {
        let priority = match illuminant {
            Illuminant::D65 => 0,
            Illuminant::D55 => 1,
            Illuminant::D50 => 2,
            Illuminant::Daylight => 3,
            Illuminant::D75 => 4,
            Illuminant::A => 5,
            _ => 6,
        };
        (priority, u16::from(illuminant))
    }

    if let Some((_, matrix)) = color_matrices
        .iter()
        .filter(|(_, matrix)| usable_flat(matrix))
        .min_by_key(|(illuminant, _)| rank(**illuminant))
    {
        let mut selected = [[0.0f32; 3]; 4];
        for (row, values) in matrix.chunks_exact(3).enumerate() {
            selected[row].copy_from_slice(values);
        }
        return Ok(selected);
    }

    if usable_legacy(&legacy) {
        return Ok(legacy);
    }

    if mono {
        let mut identity = [[0.0f32; 3]; 4];
        for channel in 0..3 {
            identity[channel][channel] = 1.0;
        }
        return Ok(identity);
    }

    Err("rawler: camera has no usable deterministic XYZ-to-camera matrix".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rawler_fallback_prefers_a_valid_d65_matrix_deterministically() {
        use rawler::imgop::xyz::Illuminant;

        let tungsten = vec![0.9, 0.1, 0.0, 0.2, 0.7, 0.1, 0.0, 0.1, 0.9];
        let d65 = vec![0.6, 0.3, 0.1, 0.1, 0.8, 0.1, 0.0, 0.2, 0.8];
        let mut matrices = std::collections::HashMap::new();
        matrices.insert(Illuminant::A, tungsten);
        matrices.insert(Illuminant::D65, d65.clone());

        let selected = select_rawler_xyz_to_cam(&matrices, [[0.0; 3]; 4], false).unwrap();
        assert_eq!(selected[0], d65[0..3]);
        assert_eq!(selected[1], d65[3..6]);
        assert_eq!(selected[2], d65[6..9]);

        // HashMap insertion/iteration order is not part of the decision.
        let mut reversed = std::collections::HashMap::new();
        reversed.insert(Illuminant::D65, d65);
        reversed.insert(
            Illuminant::A,
            vec![0.9, 0.1, 0.0, 0.2, 0.7, 0.1, 0.0, 0.1, 0.9],
        );
        assert_eq!(
            select_rawler_xyz_to_cam(&reversed, [[0.0; 3]; 4], false).unwrap(),
            selected
        );
    }

    #[test]
    fn rawler_fallback_uses_legacy_or_mono_identity_but_never_a_zero_rgb_matrix() {
        let matrices = std::collections::HashMap::new();
        let legacy = [
            [0.7, 0.2, 0.1],
            [0.1, 0.8, 0.1],
            [0.0, 0.1, 0.9],
            [0.0, 0.0, 0.0],
        ];
        assert_eq!(
            select_rawler_xyz_to_cam(&matrices, legacy, false).unwrap(),
            legacy
        );
        assert!(select_rawler_xyz_to_cam(&matrices, [[0.0; 3]; 4], false).is_err());
        assert_eq!(
            select_rawler_xyz_to_cam(&matrices, [[0.0; 3]; 4], true).unwrap(),
            [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0],
            ]
        );
    }

    /// The app's table has to answer as rawloader's does at every photosite.
    #[test]
    fn the_apps_cfa_reads_as_rawloaders_at_every_photosite() {
        let xtrans = "GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG";
        let wide = "RGBGBGRG".repeat(2);
        let names = ["RGGB", "BGGR", "GRBG", "GBRG", "RGEB", "", xtrans, &wide];
        for name in names {
            let theirs = rawloader::CFA::new(name);
            let ours = cfa_of(&theirs).unwrap();
            assert_eq!(ours.name, name);
            assert_eq!(ours.is_valid(), theirs.is_valid(), "{name:?}");
            for row in 0..150 {
                for col in 0..150 {
                    assert_eq!(
                        ours.color_at(row, col),
                        theirs.color_at(row, col),
                        "{name:?} at {row},{col}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_file_that_is_no_raw_is_an_error_naming_both_decoders() {
        let path =
            std::env::temp_dir().join(format!("iai_raw_not_a_raw_{}.cr2", std::process::id()));
        std::fs::write(&path, b"not a camera file").unwrap();
        let refused = decode_file(&path).err().expect("it is no RAW");
        let _ = std::fs::remove_file(&path);
        assert!(refused.starts_with("rawloader: "), "{refused}");
        assert!(refused.contains("; rawler: "), "{refused}");

        assert_eq!(orientation(b"not a camera file"), Orientation::Unknown);
    }
}
