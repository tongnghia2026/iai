//! PSD adjustment-layer decoding (import Phase 1 + Phase 2).
//!
//! Photoshop stores an adjustment layer's parameters as an "additional layer
//! info" block inside the layer record (keyed `levl`, `curv`, `hue2`, …) with no
//! colour pixels. The base importer used to drop those zero-pixel layers, so
//! their effect on the layers below — and their editability — was lost. Here we
//! map the well-documented legacy binary blocks onto iAi's own [`AdjustmentType`],
//! which mirrors Photoshop's parameter ranges, so a migrated adjustment lands as
//! a live, editable layer.
//!
//! Phase 1: Levels, Hue/Saturation, Brightness/Contrast, Invert, Posterize,
//! Threshold. Phase 2 adds Curves (`curv`), Channel Mixer (`mixr`), Color Balance
//! (`blnc`) and Photo Filter (`phfl`). Phase 2b adds Exposure (`expA`), Vibrance
//! (`vibA`) and Black & White (`blwh`) — the last two via the shared
//! [`super::psd_descriptor`] parser, since Photoshop stores them as descriptors.
//!
//! Blocks whose layout we don't map yet return `None`; the caller then keeps the
//! prior behaviour (skip) rather than guessing. Every parser is bounds-checked
//! and never panics on a short/truncated block.

use super::psd_descriptor::parse_versioned_descriptor;
use crate::core::layer::{identity_curve, AdjustmentType, LevelsParams};

fn be_i16(d: &[u8], off: usize) -> Option<i16> {
    d.get(off..off + 2)
        .map(|b| i16::from_be_bytes([b[0], b[1]]))
}

fn be_f32(d: &[u8], off: usize) -> Option<f32> {
    d.get(off..off + 4)
        .map(|b| f32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be_u16(d: &[u8], off: usize) -> Option<u16> {
    d.get(off..off + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn be_u32(d: &[u8], off: usize) -> Option<u32> {
    d.get(off..off + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Map a PSD "additional layer info" adjustment block to an iAi adjustment.
/// `key` is the 4-byte block key; `data` is the block body (already de-padded by
/// the caller). Returns `None` for keys/layouts not yet supported.
pub fn parse_adjustment(key: &[u8; 4], data: &[u8]) -> Option<AdjustmentType> {
    match key {
        b"levl" => parse_levels(data),
        b"curv" => parse_curves(data),
        b"hue2" => parse_hue_saturation(data),
        b"brit" => parse_brightness_contrast(data),
        b"mixr" => parse_channel_mixer(data),
        b"blnc" => parse_color_balance(data),
        b"phfl" => parse_photo_filter(data),
        b"expA" => parse_exposure(data),
        b"vibA" => parse_vibrance(data),
        b"blwh" => parse_black_and_white(data),
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

/// `curv` — Curves. Header: `u8` is-map flag, `u16` version (1 or 4), `u32`
/// channel selector. For version 1 the selector is a bitmask (bit *i* → channel
/// *i* carries a curve, curves stored in ascending bit order); for version 4 it
/// is a literal channel count with the curves stored for channels `0..count`.
/// Channel order `[composite, red, green, blue]` matches iAi's
/// `[master, red, green, blue]`; higher channels (alpha) are consumed to stay
/// aligned but dropped.
///
/// Two body forms per channel: a 256-entry output LUT (`is_map`), or a run of
/// control points. Each point is two `u16` — **output first, then input**
/// (Adobe's Curves/.acv convention), 0..255. iAi stores `(input, output)`
/// normalised to 0..1, sorted ascending by input.
fn parse_curves(d: &[u8]) -> Option<AdjustmentType> {
    let is_map = *d.first()?;
    let version = be_u16(d, 1)?;
    if version != 1 && version != 4 {
        return None;
    }
    let selector = be_u32(d, 3)?;
    // Channels to fill, in the order their curves appear in the body.
    let order: Vec<usize> = if version == 1 {
        (0..32).filter(|i| selector & (1 << i) != 0).collect()
    } else {
        (0..selector.min(32) as usize).collect()
    };

    let mut channels: [Vec<(f32, f32)>; 4] = std::array::from_fn(|_| identity_curve());
    let mut off = 7;
    for &ch in &order {
        let points = if is_map != 0 {
            let lut = d.get(off..off + 256)?;
            off += 256;
            lut_to_points(lut)
        } else {
            let n = be_u16(d, off)? as usize;
            off += 2;
            if !(2..=19).contains(&n) {
                return None;
            }
            let mut pts = Vec::with_capacity(n);
            for _ in 0..n {
                let output = be_u16(d, off)?;
                let input = be_u16(d, off + 2)?;
                off += 4;
                pts.push((input as f32 / 255.0, output as f32 / 255.0));
            }
            pts.sort_by(|a, b| a.0.total_cmp(&b.0));
            pts
        };
        if ch < 4 {
            channels[ch] = points;
        }
    }
    Some(AdjustmentType::Curves { channels })
}

/// Approximate a 256-entry output LUT as evenly spaced control points, so a
/// map-form Curves migrates to an editable (if resampled) iAi curve.
fn lut_to_points(lut: &[u8]) -> Vec<(f32, f32)> {
    let mut pts = Vec::with_capacity(18);
    let mut i = 0usize;
    while i < 256 {
        pts.push((i as f32 / 255.0, lut[i] as f32 / 255.0));
        i += 16;
    }
    if pts.last().map(|p| p.0) != Some(1.0) {
        pts.push((1.0, lut[255] as f32 / 255.0));
    }
    pts
}

/// `mixr` — Channel Mixer. Header: `u16` version (1), `u16` monochrome flag.
/// Then four 5×`i16` output records — red, green, blue, and the monochrome grey
/// output (Adobe: "RGB/CMYK colour plus constant"; GIMP reads them as
/// red/green/blue/total). Per record the first three shorts are the R/G/B source
/// weights in percent (100 = 1.0); the 4th (alpha/unused for RGB) and 5th
/// (constant) are dropped — iAi's Channel Mixer has no constant term.
fn parse_channel_mixer(d: &[u8]) -> Option<AdjustmentType> {
    let _version = be_u16(d, 0)?;
    let monochrome = be_u16(d, 2)? != 0;
    // Record r: three source weights (÷100) starting at 4 + r*10.
    let weights = |r: usize| -> Option<[f32; 3]> {
        let base = 4 + r * 10;
        Some([
            be_i16(d, base)? as f32 / 100.0,
            be_i16(d, base + 2)? as f32 / 100.0,
            be_i16(d, base + 4)? as f32 / 100.0,
        ])
    };
    if monochrome {
        // The grey output is the 4th record, held apart from the (retained)
        // colour records so a mono⇄colour toggle loses neither. Feeding it to
        // all three iAi rows makes the composed luminance equal the grey mix.
        let grey = weights(3).or_else(|| weights(0))?;
        Some(AdjustmentType::ChannelMixer {
            red: grey,
            green: grey,
            blue: grey,
            monochrome: true,
        })
    } else {
        Some(AdjustmentType::ChannelMixer {
            red: weights(0)?,
            green: weights(1)?,
            blue: weights(2)?,
            monochrome: false,
        })
    }
}

/// `blnc` — Color Balance. Three `i16` triples — shadows, midtones, highlights —
/// each `[cyan–red, magenta–green, yellow–blue]` in −100..100, then a
/// preserve-luminosity byte. The triples map straight onto iAi's per-band
/// `[R, G, B]` offsets (positive cyan–red adds red, etc.).
fn parse_color_balance(d: &[u8]) -> Option<AdjustmentType> {
    let triple = |base: usize| -> Option<[f32; 3]> {
        Some([
            be_i16(d, base)?.clamp(-100, 100) as f32,
            be_i16(d, base + 2)?.clamp(-100, 100) as f32,
            be_i16(d, base + 4)?.clamp(-100, 100) as f32,
        ])
    };
    let shadows = triple(0)?;
    let midtones = triple(6)?;
    let highlights = triple(12)?;
    let preserve_luminosity = d.get(18).copied().unwrap_or(0) != 0;
    Some(AdjustmentType::ColorBalance {
        shadows,
        midtones,
        highlights,
        preserve_luminosity,
    })
}

/// `phfl` — Photo Filter. `u16` version, then the filter colour, a `u32` density
/// (percent) and a preserve-luminosity byte. Only version 2 in the RGB colour
/// space (id 0) is mapped confidently: its four `u16` components hold the filter
/// colour R/G/B-first as 16-bit values (65535 = 255). Version 3 (CIE XYZ) and
/// non-RGB spaces have no unambiguous 8-bit RGB reading, so return `None` (skip)
/// rather than guess a wrong colour.
fn parse_photo_filter(d: &[u8]) -> Option<AdjustmentType> {
    let version = be_u16(d, 0)?;
    if version != 2 {
        return None;
    }
    if be_u16(d, 2)? != 0 {
        return None; // colour space is not RGB — no confident conversion
    }
    let comp = |i: usize| -> Option<u8> {
        let c = be_u16(d, 4 + i * 2)? as u32;
        Some(((c * 255 + 32767) / 65535) as u8)
    };
    let color = [comp(0)?, comp(1)?, comp(2)?];
    // Components end at offset 12 (2 space + 4×2); then u32 density, then a flag.
    let density = (be_u32(d, 12)? as f32 / 100.0).clamp(0.0, 1.0);
    let luminosity = d.get(16).copied().unwrap_or(0) != 0;
    Some(AdjustmentType::PhotoFilter {
        color,
        density,
        luminosity,
    })
}

/// `expA` — Exposure. `u16` version, then three big-endian `f32`: exposure
/// (stops), offset, and gamma-correction — matching iAi's Exposure fields.
fn parse_exposure(d: &[u8]) -> Option<AdjustmentType> {
    let _version = be_u16(d, 0)?;
    let exposure = be_f32(d, 2)?.clamp(-20.0, 20.0);
    let offset = be_f32(d, 6)?.clamp(-0.5, 0.5);
    let gamma = be_f32(d, 10)?.clamp(0.01, 10.0);
    Some(AdjustmentType::Exposure {
        exposure,
        offset,
        gamma,
    })
}

/// `vibA` — Vibrance. A version-prefixed descriptor with `long` keys `vibrance`
/// and `Strt` (saturation), both −100..100 — iAi's Vibrance fields.
fn parse_vibrance(d: &[u8]) -> Option<AdjustmentType> {
    let desc = parse_versioned_descriptor(d)?;
    let vibrance = desc.num("vibrance").unwrap_or(0.0) as f32;
    let saturation = desc.num("Strt").unwrap_or(0.0) as f32;
    Some(AdjustmentType::Vibrance {
        vibrance: vibrance.clamp(-100.0, 100.0),
        saturation: saturation.clamp(-100.0, 100.0),
    })
}

/// `blwh` — Black & White. A version-prefixed descriptor with a `long` per
/// colour channel (`Rd  `/`Yllw`/`Grn `/`Cyn `/`Bl  `/`Mgnt`, Photoshop range
/// −200..300; defaults 40/60/40/60/20/80) — iAi's BlackAndWhite sliders.
fn parse_black_and_white(d: &[u8]) -> Option<AdjustmentType> {
    let desc = parse_versioned_descriptor(d)?;
    let slider = |key: &str, default: f32| -> f32 {
        (desc.num(key).map(|v| v as f32).unwrap_or(default)).clamp(-200.0, 300.0)
    };
    Some(AdjustmentType::BlackAndWhite {
        r: slider("Rd  ", 40.0),
        y: slider("Yllw", 60.0),
        g: slider("Grn ", 40.0),
        c: slider("Cyn ", 60.0),
        b: slider("Bl  ", 20.0),
        m: slider("Mgnt", 80.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_i16(v: &mut Vec<u8>, x: i16) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    fn push_u16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    fn push_u32(v: &mut Vec<u8>, x: u32) {
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
    fn recognised_adjustment_keys() {
        assert!(is_adjustment_key(b"mixr"));
        assert!(is_adjustment_key(b"curv"));
        assert!(is_adjustment_key(b"blnc"));
        assert!(is_adjustment_key(b"phfl"));
        assert!(!is_adjustment_key(b"luni"));
    }

    #[test]
    fn curves_point_form_maps_bitmask_channels() {
        // Version 1, bitmask = master (bit 0) + green (bit 2). Curves appear in
        // ascending bit order: [master, green]. Red/blue stay identity.
        let mut d = Vec::new();
        d.push(0); // is_map = points
        push_u16(&mut d, 1); // version
        push_u32(&mut d, 0b0101); // bits 0 and 2
                                  // master: 3 points, lifting the midtone (input 128 → output 160).
        push_u16(&mut d, 3);
        push_u16(&mut d, 0); // out
        push_u16(&mut d, 0); // in
        push_u16(&mut d, 160); // out
        push_u16(&mut d, 128); // in
        push_u16(&mut d, 255);
        push_u16(&mut d, 255);
        // green: 2 identity endpoints.
        push_u16(&mut d, 2);
        push_u16(&mut d, 0);
        push_u16(&mut d, 0);
        push_u16(&mut d, 255);
        push_u16(&mut d, 255);

        let Some(AdjustmentType::Curves { channels }) = parse_adjustment(b"curv", &d) else {
            panic!("expected Curves");
        };
        // Master (channel 0): the lifted midpoint, stored (input, output).
        assert_eq!(channels[0].len(), 3);
        assert!((channels[0][1].0 - 128.0 / 255.0).abs() < 1e-4); // input x
        assert!((channels[0][1].1 - 160.0 / 255.0).abs() < 1e-4); // output y
                                                                  // Red (channel 1) was not in the bitmask → identity.
        assert_eq!(channels[1], identity_curve());
        // Green (channel 2) present but identity endpoints.
        assert_eq!(channels[2].len(), 2);
    }

    #[test]
    fn curves_map_form_resamples_lut() {
        // Version 4, count 1, one 256-entry inverted LUT on the master channel.
        let mut d = Vec::new();
        d.push(1); // is_map
        push_u16(&mut d, 4); // version
        push_u32(&mut d, 1); // count = 1 channel
        for i in 0..256u32 {
            d.push((255 - i) as u8); // inverted map
        }
        let Some(AdjustmentType::Curves { channels }) = parse_adjustment(b"curv", &d) else {
            panic!("expected Curves");
        };
        // First sample: input 0 → output 255; last: input 255 → output 0.
        assert!((channels[0][0].0 - 0.0).abs() < 1e-6);
        assert!((channels[0][0].1 - 1.0).abs() < 1e-4);
        let last = channels[0].last().unwrap();
        assert!((last.0 - 1.0).abs() < 1e-6);
        assert!((last.1 - 0.0).abs() < 1e-4);
    }

    #[test]
    fn channel_mixer_colour_reads_three_records() {
        let mut d = Vec::new();
        push_u16(&mut d, 1); // version
        push_u16(&mut d, 0); // monochrome = false
        let rec = |v: &mut Vec<u8>, a: i16, b: i16, c: i16| {
            push_i16(v, a);
            push_i16(v, b);
            push_i16(v, c);
            push_i16(v, 0); // 4th (unused)
            push_i16(v, 0); // constant
        };
        rec(&mut d, 100, 0, 0); // red output
        rec(&mut d, 20, 80, 0); // green output
        rec(&mut d, 0, 0, 100); // blue output
        rec(&mut d, 40, 40, 20); // grey (unused in colour mode)
        let Some(AdjustmentType::ChannelMixer {
            red,
            green,
            blue,
            monochrome,
        }) = parse_adjustment(b"mixr", &d)
        else {
            panic!("expected ChannelMixer");
        };
        assert!(!monochrome);
        assert_eq!(red, [1.0, 0.0, 0.0]);
        assert_eq!(green, [0.2, 0.8, 0.0]);
        assert_eq!(blue, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn channel_mixer_mono_uses_grey_record() {
        let mut d = Vec::new();
        push_u16(&mut d, 1);
        push_u16(&mut d, 1); // monochrome = true
        let rec = |v: &mut Vec<u8>, a: i16, b: i16, c: i16| {
            push_i16(v, a);
            push_i16(v, b);
            push_i16(v, c);
            push_i16(v, 0);
            push_i16(v, 0);
        };
        rec(&mut d, 100, 0, 0); // retained colour records
        rec(&mut d, 0, 100, 0);
        rec(&mut d, 0, 0, 100);
        rec(&mut d, 30, 59, 11); // grey mix
        let Some(AdjustmentType::ChannelMixer {
            red,
            green,
            blue,
            monochrome,
        }) = parse_adjustment(b"mixr", &d)
        else {
            panic!("expected ChannelMixer");
        };
        assert!(monochrome);
        assert_eq!(red, [0.30, 0.59, 0.11]);
        assert_eq!(red, green);
        assert_eq!(green, blue);
    }

    #[test]
    fn color_balance_reads_three_triples() {
        let mut d = Vec::new();
        push_i16(&mut d, 10); // shadows CR
        push_i16(&mut d, -5); // shadows MG
        push_i16(&mut d, 0); // shadows YB
        push_i16(&mut d, 0); // midtones
        push_i16(&mut d, 15);
        push_i16(&mut d, -20);
        push_i16(&mut d, -8); // highlights
        push_i16(&mut d, 0);
        push_i16(&mut d, 12);
        d.push(1); // preserve luminosity
        let Some(AdjustmentType::ColorBalance {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        }) = parse_adjustment(b"blnc", &d)
        else {
            panic!("expected ColorBalance");
        };
        assert_eq!(shadows, [10.0, -5.0, 0.0]);
        assert_eq!(midtones, [0.0, 15.0, -20.0]);
        assert_eq!(highlights, [-8.0, 0.0, 12.0]);
        assert!(preserve_luminosity);
    }

    #[test]
    fn photo_filter_v2_rgb_maps_colour_and_density() {
        let mut d = Vec::new();
        push_u16(&mut d, 2); // version
        push_u16(&mut d, 0); // colour space = RGB
                             // 16-bit components: 236, 138, 0 (Warming filter), 4th unused.
        push_u16(&mut d, 236 * 257);
        push_u16(&mut d, 138 * 257);
        push_u16(&mut d, 0);
        push_u16(&mut d, 0);
        push_u32(&mut d, 25); // density 25%
        d.push(1); // preserve luminosity
        let Some(AdjustmentType::PhotoFilter {
            color,
            density,
            luminosity,
        }) = parse_adjustment(b"phfl", &d)
        else {
            panic!("expected PhotoFilter");
        };
        assert_eq!(color, [236, 138, 0]);
        assert!((density - 0.25).abs() < 1e-4);
        assert!(luminosity);
    }

    #[test]
    fn photo_filter_v3_and_non_rgb_are_skipped() {
        // Version 3 (CIE XYZ) — not mapped.
        let mut v3 = Vec::new();
        push_u16(&mut v3, 3);
        v3.extend_from_slice(&[0u8; 17]);
        assert!(parse_adjustment(b"phfl", &v3).is_none());
        // Version 2 but a non-RGB colour space — not mapped.
        let mut lab = Vec::new();
        push_u16(&mut lab, 2);
        push_u16(&mut lab, 7); // Lab
        lab.extend_from_slice(&[0u8; 13]);
        assert!(parse_adjustment(b"phfl", &lab).is_none());
    }

    #[test]
    fn phase2_short_blocks_yield_none_not_panic() {
        assert!(parse_adjustment(b"curv", &[0, 0, 1]).is_none()); // header only
        assert!(parse_adjustment(b"mixr", &[0, 1, 0, 0, 0]).is_none()); // no records
        assert!(parse_adjustment(b"blnc", &[0, 0, 0, 0]).is_none()); // partial triple
        assert!(parse_adjustment(b"phfl", &[0, 2, 0, 0]).is_none()); // no components
        assert!(parse_adjustment(b"expA", &[0, 1, 0, 0]).is_none()); // truncated float
        assert!(parse_adjustment(b"vibA", &[0, 0, 0]).is_none()); // no descriptor
    }

    // --- descriptor builders for the Phase 2b tests ---

    fn push_f32(v: &mut Vec<u8>, x: f32) {
        v.extend_from_slice(&x.to_be_bytes());
    }
    fn push_desc_key(v: &mut Vec<u8>, key: &str) {
        push_u32(v, key.len() as u32);
        v.extend_from_slice(key.as_bytes());
    }
    fn push_desc_unicode(v: &mut Vec<u8>, s: &str) {
        let units: Vec<u16> = s.encode_utf16().collect();
        push_u32(v, units.len() as u32 + 1);
        for u in units {
            push_u16(v, u);
        }
        push_u16(v, 0);
    }
    /// A version-prefixed descriptor whose items are all `long`.
    fn versioned_longs(class: &str, items: &[(&str, i32)]) -> Vec<u8> {
        let mut d = Vec::new();
        push_u32(&mut d, 16); // descriptor version
        push_desc_unicode(&mut d, "");
        push_desc_key(&mut d, class);
        push_u32(&mut d, items.len() as u32);
        for (k, val) in items {
            push_desc_key(&mut d, k);
            d.extend_from_slice(b"long");
            push_u32(&mut d, *val as u32);
        }
        d
    }

    #[test]
    fn exposure_reads_three_floats() {
        let mut d = Vec::new();
        push_u16(&mut d, 1); // version
        push_f32(&mut d, 1.5); // exposure
        push_f32(&mut d, -0.05); // offset
        push_f32(&mut d, 1.2); // gamma
        let Some(AdjustmentType::Exposure {
            exposure,
            offset,
            gamma,
        }) = parse_adjustment(b"expA", &d)
        else {
            panic!("expected Exposure");
        };
        assert!((exposure - 1.5).abs() < 1e-4);
        assert!((offset - (-0.05)).abs() < 1e-4);
        assert!((gamma - 1.2).abs() < 1e-4);
    }

    #[test]
    fn vibrance_reads_descriptor() {
        let block = versioned_longs("vibrance", &[("vibrance", 40), ("Strt", -20)]);
        let Some(AdjustmentType::Vibrance {
            vibrance,
            saturation,
        }) = parse_adjustment(b"vibA", &block)
        else {
            panic!("expected Vibrance");
        };
        assert_eq!(vibrance, 40.0);
        assert_eq!(saturation, -20.0);
    }

    #[test]
    fn black_and_white_reads_six_sliders() {
        let block = versioned_longs(
            "blackAndWhite",
            &[
                ("Rd  ", 25),
                ("Yllw", 70),
                ("Grn ", 45),
                ("Cyn ", 55),
                ("Bl  ", 15),
                ("Mgnt", 90),
            ],
        );
        let Some(AdjustmentType::BlackAndWhite { r, y, g, c, b, m }) =
            parse_adjustment(b"blwh", &block)
        else {
            panic!("expected BlackAndWhite");
        };
        assert_eq!([r, y, g, c, b, m], [25.0, 70.0, 45.0, 55.0, 15.0, 90.0]);
    }

    #[test]
    fn black_and_white_missing_keys_fall_back_to_defaults() {
        // An empty descriptor → Photoshop's default mix, not a panic.
        let block = versioned_longs("blackAndWhite", &[]);
        let Some(AdjustmentType::BlackAndWhite { r, y, g, c, b, m }) =
            parse_adjustment(b"blwh", &block)
        else {
            panic!("expected BlackAndWhite");
        };
        assert_eq!([r, y, g, c, b, m], [40.0, 60.0, 40.0, 60.0, 20.0, 80.0]);
    }
}
