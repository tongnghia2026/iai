//! PSD adjustment-layer decoding (import Phase 1).
//!
//! Photoshop stores an adjustment layer's parameters as an "additional layer
//! info" block inside the layer record (keyed `levl`, `hue2`, …) with no colour
//! pixels. The base importer used to drop those zero-pixel layers, so their
//! effect on the layers below — and their editability — was lost. Here we map
//! the well-documented legacy binary blocks onto iAi's own [`AdjustmentType`],
//! which mirrors Photoshop's parameter ranges, so a migrated adjustment lands as
//! a live, editable layer.
//!
//! Blocks whose layout we don't map yet return `None`; the caller then keeps the
//! prior behaviour (skip) rather than guessing. Every parser is bounds-checked
//! and never panics on a short/truncated block.

use crate::core::layer::{AdjustmentType, LevelsParams};

fn be_i16(d: &[u8], off: usize) -> Option<i16> {
    d.get(off..off + 2)
        .map(|b| i16::from_be_bytes([b[0], b[1]]))
}

fn be_u16(d: &[u8], off: usize) -> Option<u16> {
    d.get(off..off + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

/// Map a PSD "additional layer info" adjustment block to an iAi adjustment.
/// `key` is the 4-byte block key; `data` is the block body (already de-padded by
/// the caller). Returns `None` for keys/layouts not yet supported.
pub fn parse_adjustment(key: &[u8; 4], data: &[u8]) -> Option<AdjustmentType> {
    match key {
        b"levl" => parse_levels(data),
        b"hue2" => parse_hue_saturation(data),
        b"brit" => parse_brightness_contrast(data),
        b"nvrt" => Some(AdjustmentType::Invert),
        b"post" => parse_posterize(data),
        b"thrs" => parse_threshold(data),
        _ => None,
    }
}

/// True when `key` names an adjustment block, whether or not we can map it yet.
/// Lets the caller recognise an adjustment layer (so it is not treated as an
/// empty raster) even for types Phase 1 does not decode.
pub fn is_adjustment_key(key: &[u8; 4]) -> bool {
    matches!(
        key,
        b"levl"
            | b"curv"
            | b"hue2"
            | b"hue "
            | b"brit"
            | b"nvrt"
            | b"post"
            | b"thrs"
            | b"mixr"
            | b"blnc"
            | b"phfl"
            | b"selc"
            | b"expA"
            | b"blwh"
            | b"vibA"
            | b"grdm"
    )
}

/// `levl` — Levels. `u16` version, then 29 records of five `i16`:
/// input floor, input ceiling, output floor, output ceiling, gamma (×100).
/// Record 0 is the RGB composite (master); 1/2/3 are red/green/blue — the same
/// `[master, red, green, blue]` order iAi uses.
fn parse_levels(d: &[u8]) -> Option<AdjustmentType> {
    let _version = be_u16(d, 0)?;
    let record = |i: usize| -> Option<LevelsParams> {
        let base = 2 + i * 10;
        let in_black = be_i16(d, base)?;
        let in_white = be_i16(d, base + 2)?;
        let out_black = be_i16(d, base + 4)?;
        let out_white = be_i16(d, base + 6)?;
        let gamma_raw = be_i16(d, base + 8)?;
        Some(LevelsParams {
            in_black: in_black.clamp(0, 255) as u8,
            in_white: in_white.clamp(0, 255) as u8,
            gamma: (gamma_raw as f32 / 100.0).clamp(0.1, 9.99),
            out_black: out_black.clamp(0, 255) as u8,
            out_white: out_white.clamp(0, 255) as u8,
        })
    };
    let channels = [record(0)?, record(1)?, record(2)?, record(3)?];
    Some(AdjustmentType::Levels { channels })
}

/// `hue2` — Hue/Saturation (version 2). Layout: `u16` version, colorization flag
/// byte + pad byte, then the colorization triple (3×`i16`) and the master triple
/// (3×`i16`: hue, saturation, lightness) before the six per-range records. iAi's
/// Hue/Saturation is master-only, so we take the master triple.
fn parse_hue_saturation(d: &[u8]) -> Option<AdjustmentType> {
    let _version = be_u16(d, 0)?;
    // 2: colorization flag, 3: padding, 4..10: colorization triple, 10..16: master.
    let hue = be_i16(d, 10)? as f32;
    let saturation = be_i16(d, 12)? as f32;
    let lightness = be_i16(d, 14)? as f32;
    Some(AdjustmentType::HueSaturation {
        hue: hue.clamp(-180.0, 180.0),
        saturation: saturation.clamp(-100.0, 100.0),
        lightness: lightness.clamp(-100.0, 100.0),
    })
}

/// `brit` — legacy Brightness/Contrast: `i16` brightness, `i16` contrast (both
/// −100..100), then a mean byte and a flag we don't need.
fn parse_brightness_contrast(d: &[u8]) -> Option<AdjustmentType> {
    let brightness = be_i16(d, 0)? as f32;
    let contrast = be_i16(d, 2)? as f32;
    Some(AdjustmentType::BrightnessContrast {
        brightness: brightness.clamp(-100.0, 100.0),
        contrast: contrast.clamp(-100.0, 100.0),
    })
}

/// `post` — Posterize: `i16` level count (2..255).
fn parse_posterize(d: &[u8]) -> Option<AdjustmentType> {
    let levels = be_i16(d, 0)?.clamp(2, 255) as u8;
    Some(AdjustmentType::Posterize { levels })
}

/// `thrs` — Threshold: `i16` level (1..255).
fn parse_threshold(d: &[u8]) -> Option<AdjustmentType> {
    let value = be_i16(d, 0)?.clamp(1, 255) as u8;
    Some(AdjustmentType::Threshold { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_i16(v: &mut Vec<u8>, x: i16) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    #[test]
    fn levels_maps_master_and_rgb_records() {
        let mut d = Vec::new();
        push_i16(&mut d, 2); // version
                             // 29 records; set master (0) + red (1), leave the rest identity.
        for i in 0..29 {
            match i {
                0 => {
                    push_i16(&mut d, 10); // in_black
                    push_i16(&mut d, 245); // in_white
                    push_i16(&mut d, 5); // out_black
                    push_i16(&mut d, 250); // out_white
                    push_i16(&mut d, 120); // gamma ×100 → 1.20
                }
                1 => {
                    push_i16(&mut d, 20);
                    push_i16(&mut d, 200);
                    push_i16(&mut d, 0);
                    push_i16(&mut d, 255);
                    push_i16(&mut d, 90); // 0.90
                }
                _ => {
                    push_i16(&mut d, 0);
                    push_i16(&mut d, 255);
                    push_i16(&mut d, 0);
                    push_i16(&mut d, 255);
                    push_i16(&mut d, 100);
                }
            }
        }
        let Some(AdjustmentType::Levels { channels }) = parse_adjustment(b"levl", &d) else {
            panic!("expected Levels");
        };
        assert_eq!(channels[0].in_black, 10);
        assert_eq!(channels[0].in_white, 245);
        assert_eq!(channels[0].out_black, 5);
        assert_eq!(channels[0].out_white, 250);
        assert!((channels[0].gamma - 1.20).abs() < 1e-4);
        assert_eq!(channels[1].in_black, 20);
        assert!((channels[1].gamma - 0.90).abs() < 1e-4);
        assert_eq!(channels[2].in_black, 0); // green identity
    }

    #[test]
    fn hue_saturation_takes_master_triple() {
        let mut d = Vec::new();
        push_i16(&mut d, 2); // version
        d.push(0); // colorization flag
        d.push(0); // pad
        push_i16(&mut d, 0); // colorization hue
        push_i16(&mut d, 0); // colorization sat
        push_i16(&mut d, 0); // colorization light
        push_i16(&mut d, -40); // master hue
        push_i16(&mut d, 25); // master sat
        push_i16(&mut d, -10); // master light
        let Some(AdjustmentType::HueSaturation {
            hue,
            saturation,
            lightness,
        }) = parse_adjustment(b"hue2", &d)
        else {
            panic!("expected HueSaturation");
        };
        assert_eq!(hue, -40.0);
        assert_eq!(saturation, 25.0);
        assert_eq!(lightness, -10.0);
    }

    #[test]
    fn brightness_contrast_reads_two_shorts() {
        let mut d = Vec::new();
        push_i16(&mut d, 30);
        push_i16(&mut d, -15);
        push_i16(&mut d, 127); // mean
        d.push(0); // lab flag
        let Some(AdjustmentType::BrightnessContrast {
            brightness,
            contrast,
        }) = parse_adjustment(b"brit", &d)
        else {
            panic!("expected BrightnessContrast");
        };
        assert_eq!(brightness, 30.0);
        assert_eq!(contrast, -15.0);
    }

    #[test]
    fn invert_posterize_threshold() {
        assert!(matches!(
            parse_adjustment(b"nvrt", &[]),
            Some(AdjustmentType::Invert)
        ));
        let mut p = Vec::new();
        push_i16(&mut p, 5);
        assert!(matches!(
            parse_adjustment(b"post", &p),
            Some(AdjustmentType::Posterize { levels: 5 })
        ));
        let mut t = Vec::new();
        push_i16(&mut t, 90);
        assert!(matches!(
            parse_adjustment(b"thrs", &t),
            Some(AdjustmentType::Threshold { value: 90 })
        ));
    }

    #[test]
    fn short_block_yields_none_not_panic() {
        assert!(parse_adjustment(b"levl", &[0, 2]).is_none()); // version only
        assert!(parse_adjustment(b"hue2", &[0, 2, 0, 0]).is_none()); // no master triple
        assert!(parse_adjustment(b"brit", &[0]).is_none());
    }

    #[test]
    fn unknown_key_is_none_but_recognised_as_adjustment() {
        assert!(parse_adjustment(b"mixr", &[0; 32]).is_none());
        assert!(is_adjustment_key(b"mixr"));
        assert!(is_adjustment_key(b"curv"));
        assert!(!is_adjustment_key(b"luni"));
    }
}
