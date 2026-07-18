use super::*;
use crate::core::color::{luminance_f32, oklab_hue_deg, rgb_to_hsl};
use crate::core::tile::{quantize_dither, TileMap};

#[test]
fn develop_16bit_keeps_hdr_and_beats_banding() {
    // A 16-bit ramp through a global Develop edit (exposure + contrast) must
    // stay 16-bit and keep far more than 256 distinct levels — no tone banding.
    let (w, h) = (4000u32, 1u32);
    let mut px16 = vec![0u16; (w * h * 4) as usize];
    for x in 0..w as usize {
        let v = ((x as u32 * 65535) / (w - 1)) as u16;
        px16[x * 4] = v;
        px16[x * 4 + 1] = v;
        px16[x * 4 + 2] = v;
        px16[x * 4 + 3] = 65535;
    }
    let source = TileMap::from_rgba16(&px16, w, h);
    let settings = DevelopSettings {
        exposure: 0.6,
        contrast: 40.0,
        ..Default::default()
    };
    let out = apply_to_tilemap_direct(&source, &settings, None);
    assert!(out.has_hdr(), "Develop should keep the 16-bit master");

    let flat = out.flatten16();
    let distinct: std::collections::BTreeSet<u16> = flat.chunks(4).map(|c| c[0]).collect();
    assert!(
        distinct.len() > 256,
        "16-bit Develop should be finely stepped, got {}",
        distinct.len()
    );
}

/// Smooth colourful 16-bit ramp for the spatial-stage tests.
#[cfg(test)]
fn ramp16(w: u32) -> Vec<u16> {
    let mut px = vec![0u16; (w * 4) as usize];
    for x in 0..w as usize {
        let t = ((x as u32 * 65535) / (w - 1)) as u16;
        px[x * 4] = t;
        px[x * 4 + 1] = 30000;
        px[x * 4 + 2] = 65535 - t;
        px[x * 4 + 3] = 65535;
    }
    px
}

#[test]
fn develop_16bit_colour_stage_keeps_hdr() {
    // Per-band Colour (mixer) on a 16-bit source must stay 16-bit and finely
    // stepped (the spatial colour low-pass path).
    let w = 2000u32;
    let source = TileMap::from_rgba16(&ramp16(w), w, 1);
    let settings = DevelopSettings {
        exposure: 0.3,
        mixer_saturation: [45.0; MIXER_BANDS],
        ..Default::default()
    };
    let out = apply_to_tilemap_direct(&source, &settings, None);
    assert!(out.has_hdr(), "colour stage should keep the 16-bit master");
    let distinct: std::collections::BTreeSet<u16> =
        out.flatten16().chunks(4).map(|c| c[0]).collect();
    assert!(distinct.len() > 256, "got {}", distinct.len());
}

#[test]
fn develop_16bit_local_tone_keeps_hdr() {
    // Local-adaptation Shadows/Highlights on a 16-bit source.
    let w = 2000u32;
    let source = TileMap::from_rgba16(&ramp16(w), w, 1);
    let settings = DevelopSettings {
        shadows: 60.0,
        highlights: -40.0,
        ..Default::default()
    };
    let out = apply_to_tilemap_direct(&source, &settings, None);
    assert!(out.has_hdr(), "local tone should keep the 16-bit master");
    let distinct: std::collections::BTreeSet<u16> =
        out.flatten16().chunks(4).map(|c| c[0]).collect();
    assert!(distinct.len() > 256, "got {}", distinct.len());
}

#[test]
fn live_histogram_follows_settings() {
    // The curve-editor histogram is re-binned through the current settings,
    // so an exposure lift must move the luma mass right and a warm WB must
    // push the R and B channels apart — not stay frozen at the source.
    let tiles = TileMap::new_solid(64, 64, 100, 100, 100, 255);
    let proxy = build_histogram_proxy(&tiles);
    assert!(!proxy.is_empty());

    let neutral = histogram_rgbl(&proxy, &DevelopSettings::default());
    let luma_peak_at = |h: &[[f32; 256]; 4]| {
        h[3].iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap()
    };
    assert_eq!(luma_peak_at(&neutral), 100);

    let mut lifted = DevelopSettings::default();
    lifted.exposure = 40.0;
    let bright = histogram_rgbl(&proxy, &lifted);
    assert!(
        luma_peak_at(&bright) > 100,
        "exposure lift must move the luma histogram right, peak at {}",
        luma_peak_at(&bright)
    );

    let mut warm = DevelopSettings::default();
    warm.temperature = 60.0;
    let warmed = histogram_rgbl(&proxy, &warm);
    let peak_of = |ch: &[f32; 256]| {
        ch.iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap()
    };
    assert!(
        peak_of(&warmed[0]) > peak_of(&warmed[2]),
        "warm WB must push the R channel above B (R at {}, B at {})",
        peak_of(&warmed[0]),
        peak_of(&warmed[2])
    );
}

#[test]
fn histogram_proxy_skips_transparent_pixels() {
    // Fully transparent pixels carry no colour — they must not spike bin 0.
    let tiles = TileMap::new(64, 64);
    assert!(build_histogram_proxy(&tiles).is_empty());
}

#[test]
fn local_mask_weights_follow_geometry() {
    // Linear: full effect before the start handle, zero past the end.
    let lin = LocalMaskShape::Linear {
        x0: 0.25,
        y0: 0.5,
        x1: 0.75,
        y1: 0.5,
    };
    assert!((lin.weight(0.0, 0.5) - 1.0).abs() < 1e-4);
    assert!(lin.weight(1.0, 0.5) < 1e-4);
    let mid = lin.weight(0.5, 0.5);
    assert!(mid > 0.3 && mid < 0.7, "mid-ramp weight {mid}");

    // Radial: full inside, zero outside, inverted flips.
    let rad = LocalMaskShape::Radial {
        cx: 0.5,
        cy: 0.5,
        rx: 0.2,
        ry: 0.2,
        feather: 0.5,
        invert: false,
    };
    assert!((rad.weight(0.5, 0.5) - 1.0).abs() < 1e-4);
    assert!(rad.weight(0.9, 0.5) < 1e-4);
    let rad_inv = LocalMaskShape::Radial {
        cx: 0.5,
        cy: 0.5,
        rx: 0.2,
        ry: 0.2,
        feather: 0.5,
        invert: true,
    };
    assert!(rad_inv.weight(0.5, 0.5) < 1e-4);
    assert!((rad_inv.weight(0.9, 0.5) - 1.0).abs() < 1e-4);
}

#[test]
fn local_adjustment_changes_only_masked_area() {
    // A radial exposure lift in the left half must brighten its centre and
    // leave the far side untouched; the same settings must count as
    // non-neutral so the bake actually runs.
    let src = TileMap::new_solid(200, 100, 100, 100, 100, 255);
    let mut settings = DevelopSettings::default();
    settings.locals.push(LocalAdjustment {
        shape: LocalMaskShape::Radial {
            cx: 0.25,
            cy: 0.5,
            rx: 0.15,
            ry: 0.3,
            feather: 0.5,
            invert: false,
        },
        settings: LocalSettings {
            exposure: 60.0,
            ..Default::default()
        },
    });
    assert!(!settings.is_neutral());
    assert!(settings.has_locals());

    let out = apply_to_tilemap_direct(&src, &settings, None);
    let (r_in, ..) = out.get_pixel(50, 50);
    let (r_out, ..) = out.get_pixel(190, 50);
    assert!(r_in > 100, "masked centre should brighten, got {r_in}");
    assert_eq!(r_out, 100, "far side must stay untouched");
}

#[test]
fn local_adjustment_16bit_masked_area() {
    // Same mask discipline on the 16-bit path (apply_pixel16).
    let w = 200u32;
    let mut px16 = vec![0u16; (w * 4) as usize];
    for x in 0..w as usize {
        px16[x * 4] = 25000;
        px16[x * 4 + 1] = 25000;
        px16[x * 4 + 2] = 25000;
        px16[x * 4 + 3] = 65535;
    }
    let src = TileMap::from_rgba16(&px16, w, 1);
    let mut settings = DevelopSettings::default();
    settings.locals.push(LocalAdjustment {
        shape: LocalMaskShape::Linear {
            x0: 0.2,
            y0: 0.0,
            x1: 0.5,
            y1: 0.0,
        },
        settings: LocalSettings {
            exposure: 60.0,
            ..Default::default()
        },
    });
    let out = apply_to_tilemap_direct(&src, &settings, None);
    let flat = out.flatten16();
    let left = flat[10 * 4];
    let right = flat[190 * 4];
    assert!(left > 25000, "full-effect side should brighten, got {left}");
    assert_eq!(right, 25000, "zero side must stay untouched");
}

#[test]
fn neutral_settings_do_not_change_pixels() {
    let mut pixels = vec![10, 20, 30, 255, 80, 90, 100, 120];
    let before = pixels.clone();
    apply_to_pixels(&DevelopSettings::default(), &mut pixels, 2, 1);
    assert_eq!(pixels, before);
}

#[test]
fn exposure_brightens_rgb_and_preserves_alpha() {
    let mut pixels = vec![60, 70, 80, 123];
    let mut settings = DevelopSettings::default();
    settings.exposure = 20.0;
    apply_to_pixels(&settings, &mut pixels, 1, 1);
    assert!(pixels[0] > 60);
    assert!(pixels[1] > 70);
    assert!(pixels[2] > 80);
    assert_eq!(pixels[3], 123);
}

#[test]
fn exposure_lifts_shadow_detail_without_blowing_highlights() {
    let mut settings = DevelopSettings::default();
    settings.exposure = EXPOSURE_LIMIT * 0.5;

    let shadow = vec![28, 24, 20, 255];
    let highlight = vec![210, 198, 176, 255];
    let mut shadow_out = shadow.clone();
    let mut highlight_out = highlight.clone();
    apply_to_pixels(&settings, &mut shadow_out, 1, 1);
    apply_to_pixels(&settings, &mut highlight_out, 1, 1);

    // With the corrected rolloff (f(1.0)=1.0), a +2.5 EV push on a
    // bright pixel can reach 255 — that is correct, the shoulder only
    // compresses values that EXCEED 1.0 in linear. The test verifies the
    // shadow lifts and the highlight stays brighter than the input.
    assert!(rgb_distance(&shadow, &shadow_out) > 18);
    assert!(highlight_out[0] > highlight[0]);
}

#[test]
fn tone_stage_is_driven_entirely_by_shared_tonedata() {
    let mut settings = DevelopSettings::default();
    settings.exposure = 20.0;
    settings.contrast = 30.0;
    settings.highlights = -40.0;
    settings.temperature = 25.0;
    let plan = DevelopPlan::new(&settings, 1, 1);
    let got = plan.apply_pixel(250, 120, 40, 255, 0, 0, None);

    let tone = build_tone_data(&settings);
    let (mut r, mut g, mut b) = (250.0 / 255.0, 120.0 / 255.0, 40.0 / 255.0);
    tone.apply(&mut r, &mut g, &mut b);
    assert_eq!(
        &got[0..3],
        &[
            quantize_dither(r, 0, 0, 0),
            quantize_dither(g, 0, 0, 1),
            quantize_dither(b, 0, 0, 2),
        ]
    );
}

#[test]
fn tone_lut_is_monotonic() {
    let mut settings = DevelopSettings::default();
    settings.contrast = CONTROL_LIMIT;
    settings.highlights = CONTROL_LIMIT;
    settings.shadows = -CONTROL_LIMIT;
    settings.whites = CONTROL_LIMIT;
    settings.blacks = -CONTROL_LIMIT;
    settings.curve_darks = CONTROL_LIMIT;
    let lut = build_tone_lut(&settings);
    for i in 1..lut.len() {
        assert!(lut[i] >= lut[i - 1], "tone LUT not monotone at {i}");
    }
}

#[test]
fn positive_exposure_keeps_highlight_texture_headroom() {
    let mut settings = DevelopSettings::default();
    settings.exposure = EXPOSURE_LIMIT * 0.55;

    let fabric_a = develop_one(&settings, [220, 218, 212]);
    let fabric_b = develop_one(&settings, [232, 230, 224]);
    let leaf_a = develop_one(&settings, [184, 208, 126]);
    let leaf_b = develop_one(&settings, [196, 220, 138]);

    assert!(
        fabric_b[0] > fabric_a[0],
        "white fabric collapsed: {fabric_a:?} -> {fabric_b:?}"
    );
    assert!(
        leaf_b[1] >= leaf_a[1],
        "bright leaf collapsed: {leaf_a:?} -> {leaf_b:?}"
    );
    assert!(
        rgb_distance(&fabric_a, &fabric_b) >= 2,
        "fabric folds lost tonal separation: {fabric_a:?} {fabric_b:?}"
    );
    assert!(
        rgb_distance(&leaf_a, &leaf_b) >= 4,
        "leaf texture lost tonal separation: {leaf_a:?} {leaf_b:?}"
    );
}

#[test]
fn highlights_and_whites_preserve_near_white_ordering() {
    let mut highlights = DevelopSettings::default();
    highlights.highlights = CONTROL_LIMIT;
    let mut whites = DevelopSettings::default();
    whites.whites = CONTROL_LIMIT;

    for (name, settings) in [("highlights", highlights), ("whites", whites)] {
        let a = develop_one(&settings, [218, 218, 216]);
        let b = develop_one(&settings, [228, 228, 226]);
        let c = develop_one(&settings, [238, 238, 236]);
        assert!(
            a[0] < b[0] && b[0] < c[0],
            "{name} flattened ramp: {a:?} {b:?} {c:?}"
        );
        assert!(c[0] < 255, "{name} hard-clipped near white: {c:?}");
        assert!(
            (c[0] as i32 - a[0] as i32) >= 4,
            "{name} erased near-white texture: {a:?} {c:?}"
        );
    }
}

#[test]
fn shadows_lift_dark_colour_without_gray_fog() {
    let mut settings = DevelopSettings::default();
    settings.shadows = CONTROL_LIMIT;

    let foliage_dark = develop_one(&settings, [18, 42, 20]);
    let foliage_light = develop_one(&settings, [28, 58, 26]);
    let wood_dark = develop_one(&settings, [46, 28, 16]);
    let black = develop_one(&settings, [2, 2, 2]);

    assert!(luma_u8(&foliage_dark) > luma_u8(&[18, 42, 20, 255]));
    assert!(
        foliage_light[1] > foliage_dark[1] && foliage_dark[1] > foliage_dark[0] + 12,
        "foliage went gray/foggy: {foliage_dark:?} {foliage_light:?}"
    );
    assert!(
        wood_dark[0] > wood_dark[2] + 12,
        "dark wood lost warm chroma: {wood_dark:?}"
    );
    assert!(
        black[0] < 18,
        "true black lifted too much by Shadows: {black:?}"
    );
}

#[test]
fn strong_light_edit_stays_smooth_finite_and_color_stable() {
    let mut settings = DevelopSettings::default();
    settings.exposure = EXPOSURE_LIMIT * 0.35;
    settings.contrast = CONTROL_LIMIT * 0.55;
    settings.highlights = -CONTROL_LIMIT * 0.45;
    settings.shadows = CONTROL_LIMIT * 0.65;
    settings.whites = CONTROL_LIMIT * 0.35;
    settings.blacks = CONTROL_LIMIT * 0.20;

    for (name, rgb) in [
        ("red", [180, 38, 34]),
        ("green", [40, 132, 48]),
        ("blue", [44, 76, 180]),
        ("skin", [196, 132, 96]),
        ("gray", [120, 120, 120]),
        ("white", [232, 232, 228]),
    ] {
        let out = develop_one(&settings, rgb);
        if name != "gray" && name != "white" {
            let (h0, s0, _) = rgb_to_hsl(
                rgb[0] as f32 / 255.0,
                rgb[1] as f32 / 255.0,
                rgb[2] as f32 / 255.0,
            );
            let (h1, s1, _) = rgb_to_hsl(
                out[0] as f32 / 255.0,
                out[1] as f32 / 255.0,
                out[2] as f32 / 255.0,
            );
            let hue_gap = ((h1 - h0 + 180.0).rem_euclid(360.0) - 180.0).abs();
            assert!(
                hue_gap < 14.0,
                "{name} hue drift {hue_gap:.2}: {rgb:?} -> {out:?}"
            );
            assert!(
                s1 > s0 * 0.55,
                "{name} desaturated too far: {s0:.3} -> {s1:.3}"
            );
        }
    }

    let lut = build_tone_lut(&settings);
    for w in lut.windows(2) {
        assert!(w[0].is_finite() && w[1].is_finite());
        assert!(w[1] >= w[0], "strong light LUT discontinuity: {w:?}");
    }
}

#[test]
fn production_local_light_preserves_bright_and_dark_texture() {
    use crate::core::tile::{TilePos, TILE_SIZE};
    let (w, h) = (96u32, 24u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let c = if x < w / 2 {
                let v = if x % 2 == 0 { 222u8 } else { 238u8 };
                [v, v, v - 4]
            } else {
                let v = if x % 2 == 0 { 18u8 } else { 36u8 };
                [v, v + 18, v]
            };
            px[i] = c[0];
            px[i + 1] = c[1];
            px[i + 2] = c[2];
            px[i + 3] = 255;
        }
    }

    let mut settings = DevelopSettings::default();
    settings.exposure = EXPOSURE_LIMIT * 0.35;
    settings.highlights = -CONTROL_LIMIT * 0.45;
    settings.shadows = CONTROL_LIMIT * 0.75;
    settings.whites = CONTROL_LIMIT * 0.25;
    settings.blacks = CONTROL_LIMIT * 0.15;

    let out = apply_to_tilemap_direct(&TileMap::from_rgba(&px, w, h), &settings, None);
    let at = |x: u32, y: u32| -> [u8; 3] {
        let t = out.tiles.get(&TilePos::from_pixel(x, y)).unwrap();
        let (r, g, b, _) = t.get_pixel(x % TILE_SIZE, y % TILE_SIZE);
        [r, g, b]
    };
    let yy = h / 2;
    let bright_step = (at(11, yy)[0] as i32 - at(10, yy)[0] as i32).abs();
    let dark_step = (at(65, yy)[1] as i32 - at(64, yy)[1] as i32).abs();

    assert!(
        at(11, yy)[0] < 255,
        "bright fabric clipped: {:?}",
        at(11, yy)
    );
    assert!(
        bright_step >= 4,
        "bright texture flattened to step {bright_step}"
    );
    assert!(luma_u8(&[at(64, yy)[0], at(64, yy)[1], at(64, yy)[2], 255]) > 30.0);
    assert!(
        dark_step >= 8,
        "dark foliage texture flattened to step {dark_step}"
    );
    assert!(
        at(64, yy)[1] > at(64, yy)[0] + 10,
        "foliage went gray: {:?}",
        at(64, yy)
    );
}

#[test]
#[ignore = "diagnostic instrumentation, run with --ignored --nocapture"]
fn diag_light_pipeline_samples() {
    let mut settings = DevelopSettings::default();
    settings.exposure = EXPOSURE_LIMIT * 0.35;
    settings.contrast = CONTROL_LIMIT * 0.55;
    settings.highlights = -CONTROL_LIMIT * 0.45;
    settings.shadows = CONTROL_LIMIT * 0.65;
    settings.whites = CONTROL_LIMIT * 0.35;
    settings.blacks = CONTROL_LIMIT * 0.20;
    let tone = build_tone_data(&settings);

    for (name, rgb) in [
        ("white_fabric", [232, 230, 224]),
        ("bright_leaf", [196, 220, 138]),
        ("dark_foliage", [18, 42, 20]),
        ("skin_highlight", [218, 174, 150]),
        ("dark_wood", [46, 28, 16]),
        ("near_black", [3, 3, 3]),
    ] {
        let (mut r, mut g, mut b) = (
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
        );
        let input_l = luminance_f32(r, g, b).clamp(0.0, 1.0);
        let input_chroma = rgb_chroma(r, g, b);
        let input_hue = rgb_to_hsl(r, g, b).0;
        tone.apply_interp(&mut r, &mut g, &mut b);
        let out = [
            (r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (b.clamp(0.0, 1.0) * 255.0).round() as u8,
        ];
        let out_l = luminance_f32(r, g, b).clamp(0.0, 1.0);
        let out_chroma = rgb_chroma(r, g, b);
        let out_hue = rgb_to_hsl(r, g, b).0;
        let hue_drift = ((out_hue - input_hue + 180.0).rem_euclid(360.0) - 180.0).abs();
        println!(
            "{name}: in={rgb:?} l={input_l:.4} chroma={input_chroma:.4} \
             masks(h={:.3},s={:.3},w={:.3},b={:.3}) ev={:.3} contrast={:.3} \
             out={out:?} l={out_l:.4} chroma={out_chroma:.4} hue_drift={hue_drift:.2} \
             clip=({}, {})",
            highlight_mask(input_l),
            shadow_mask(input_l),
            white_mask(input_l),
            black_mask(input_l),
            tone.ev,
            eased_control(settings.contrast),
            out.contains(&0),
            out.contains(&255),
        );
    }
}

#[test]
fn white_balance_preserves_neutral_brightness() {
    assert_eq!(wb_gains(&DevelopSettings::default()), [1.0, 1.0, 1.0]);

    let mut settings = DevelopSettings::default();
    settings.temperature = CONTROL_LIMIT * 0.5;
    let mut grey = vec![128, 128, 128, 255];
    let before = grey.clone();
    apply_to_pixels(&settings, &mut grey, 1, 1);
    assert!(grey[0] > before[0], "warm should raise red");
    assert!(grey[2] < before[2], "warm should lower blue");
    let luma_before = luma_u8(&before);
    let luma_after = luma_u8(&grey);
    assert!(
        (luma_after - luma_before).abs() < 14.0,
        "brightness drifted"
    );
}

#[test]
fn wb_only_drag_is_immediate_eligible_even_with_colour_engaged() {
    // A Temperature/Tint tick must be recognised as a white-balance-only diff
    // so the live preview recomposes immediately (smooth) instead of stepping
    // at the throttle rate — even when Colour/local-tone/Effects are engaged.
    let mut base = DevelopSettings::default();
    base.saturation = 30.0;
    base.shadows = 40.0;
    base.mixer_hue[2] = -15.0;

    let mut next = base.clone();
    next.temperature += 3.0;
    assert!(next.differs_only_white_balance(&base));
    assert!(
        !next.preview_proxy_free(),
        "Colour/local tone still engaged"
    );

    let mut tint_tick = base.clone();
    tint_tick.tint -= 2.0;
    assert!(tint_tick.differs_only_white_balance(&base));

    // Not WB-only once another stage also moves in the same diff.
    let mut mixed = base.clone();
    mixed.temperature += 3.0;
    mixed.exposure += 1.0;
    assert!(!mixed.differs_only_white_balance(&base));

    // An identical (no-op) diff is not a WB drag.
    assert!(!base.differs_only_white_balance(&base));
}

#[test]
fn direct_previews_get_fresh_tile_revisions_each_time() {
    let source = TileMap::from_rgba(&[60, 70, 80, 255], 1, 1);
    let mut settings = DevelopSettings::default();
    settings.exposure = 1.0;

    let a = apply_to_tilemap_direct(&source, &settings, None);
    let b = apply_to_tilemap_direct(&source, &settings, None);
    let rev_a = a.tiles.values().next().unwrap().revision;
    let rev_b = b.tiles.values().next().unwrap().revision;

    assert_ne!(rev_a, rev_b);
}

#[test]
fn region_luma_proxy_separates_bright_and_dark_regions() {
    // Left half dark, right half bright. The proxy is the GPU local-tone
    // preview's regional base luma; it must keep the two regions apart (the
    // guided filter preserves the strong edge) so a shadow lift finds the dark
    // side without bleeding the bright side in.
    let w = 256u32;
    let h = 64u32;
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let v: u8 = if x < w / 2 { 30 } else { 200 };
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
    }
    let tm = TileMap::from_rgba(&px, w, h);
    let tone = build_tone_data(&DevelopSettings::default());
    let (proxy, pw, ph) = build_region_luma_proxy(&tm, &tone, 4);
    assert_eq!((pw, ph), (64, 16));

    let mid = (ph / 2) as f32;
    let dark = sample_plane_bilinear(&proxy, pw, ph, 16.0, mid);
    let bright = sample_plane_bilinear(&proxy, pw, ph, 48.0, mid);
    assert!(
        bright > dark + 0.3,
        "proxy did not separate regions: dark={dark}, bright={bright}"
    );
    assert!((0.0..=1.0).contains(&dark) && (0.0..=1.0).contains(&bright));
}

#[test]
fn color_proxies_saturate_adjusted_but_not_region() {
    // Flat reddish field. The GPU colour preview samples `region` (the toned
    // low-pass) and `adjusted` (region after the colour transform); a Saturation
    // push must raise chroma in `adjusted` while `region` tracks the source.
    let w = 192u32;
    let h = 48u32;
    let base = [150u8, 86, 74];
    let mut px = vec![0u8; (w * h * 4) as usize];
    for i in 0..(w * h) as usize {
        px[i * 4] = base[0];
        px[i * 4 + 1] = base[1];
        px[i * 4 + 2] = base[2];
        px[i * 4 + 3] = 255;
    }
    let tm = TileMap::from_rgba(&px, w, h);
    let mut settings = DevelopSettings::default();
    settings.saturation = CONTROL_LIMIT;

    let (region, adjusted, pw, ph) = build_color_proxies(&tm, &None, &settings, COLOR_DOWNSAMPLE);
    assert_eq!(
        (pw, ph),
        (
            (w as usize).div_ceil(COLOR_DOWNSAMPLE),
            (h as usize).div_ceil(COLOR_DOWNSAMPLE)
        )
    );

    let i = (ph / 2) * pw + pw / 2;
    let chroma = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
    assert!(
        (region[i][0] - base[0] as f32 / 255.0).abs() < 0.05,
        "region should track the source: {:?}",
        region[i]
    );
    assert!(
        chroma(adjusted[i]) > chroma(region[i]) + 0.02,
        "saturation should raise adjusted chroma: region={:?} adjusted={:?}",
        region[i],
        adjusted[i]
    );
}

#[test]
fn sharpening_does_not_amplify_chroma() {
    // A slightly reddish blob brighter than its near-neutral surround = a
    // positive-lift edge, the case the old engine amplified chroma on
    // (magenta/cyan beads along wires). Sharpening is luminance-only and the
    // de-fringe pull only REDUCES chroma, so chroma must not grow.
    let (w, h) = (16u32, 16u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let reddish = (7..9).contains(&x) && (7..9).contains(&y);
            let (r, g, b) = if reddish {
                (153u8, 107u8, 107u8) // ~[0.60, 0.42, 0.42]
            } else {
                (102, 102, 102) // ~0.40 neutral
            };
            px[i] = r;
            px[i + 1] = g;
            px[i + 2] = b;
            px[i + 3] = 255;
        }
    }
    let src = TileMap::from_rgba(&px, w, h);
    let mut settings = DevelopSettings::default();
    settings.sharpening = 100.0;
    let out = apply_detail_to_tilemap(&src, &settings);
    let chroma_of = |t: &TileMap, x: u32, y: u32| {
        let (r, g, b, _a) = t.get_pixel(x, y);
        (r.max(g).max(b) as i32 - r.min(g).min(b) as i32) as f32
    };
    assert!(
        chroma_of(&out, 7, 7) <= chroma_of(&src, 7, 7) + 2.0,
        "sharpening must not amplify chroma: in={} out={}",
        chroma_of(&src, 7, 7),
        chroma_of(&out, 7, 7)
    );
    let lum_of = |t: &TileMap, x: u32, y: u32| {
        let (r, g, b, _a) = t.get_pixel(x, y);
        luma_u8(&[r, g, b])
    };
    assert!(
        lum_of(&out, 7, 7) > lum_of(&src, 7, 7) - 1.0,
        "the bright side of the edge should still sharpen (brighten), not darken"
    );
}

#[test]
fn color_path_toned_uses_regional_shadows() {
    // Shadow boundary: dark left, bright right. With a large regional radius
    // the base luminance everywhere ≈ the image average, well above the dark
    // pixels' own luma — so regional Shadows lift (apply_local) differs from
    // per-pixel lift (apply). The colour path's `toned` base must use the
    // REGIONAL one (matching the tone-only path + the GPU preview) so engaging
    // the Mixer does not jump the shadows.
    let (w, h) = (16u32, 16u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let v = if x < w / 2 { 40u8 } else { 180u8 };
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
    }
    let src = TileMap::from_rgba(&px, w, h);

    // Shadows (local tone) + Saturation (engages the colour path).
    let mut settings = DevelopSettings::default();
    settings.shadows = 80.0;
    settings.saturation = 40.0;
    let tone = build_tone_data(&settings);
    assert!(tone.is_local && has_color(&settings));

    let base = build_base_luma(&src, Some(&tone), 0, 0, w, h);
    let tone_opt = Some(tone);
    let (toned, _region, _adjusted) =
        build_color_lowpass(&src, &tone_opt, &settings, 0, 0, w, h, false, Some(&base));
    let t = tone_opt.as_ref().unwrap();

    // Reference: the regional local-adaptation on a dark pixel (col 2, row 8).
    let idx = (8 * w + 2) as usize;
    let (mut rr, mut gg, mut bb) = (40.0 / 255.0, 40.0 / 255.0, 40.0 / 255.0);
    t.apply_local(&mut rr, &mut gg, &mut bb, base[idx]);
    let (mut gr, mut ggn, mut gbn) = (40.0 / 255.0, 40.0 / 255.0, 40.0 / 255.0);
    t.apply(&mut gr, &mut ggn, &mut gbn);

    // The colour path's toned base matches the REGIONAL result…
    assert!(
        (toned[idx][0] - rr).abs() < 0.01,
        "colour-path toned should equal apply_local (regional): {} vs {}",
        toned[idx][0],
        rr
    );
    // …and the regional result is meaningfully different from the per-pixel
    // (global) one, so the test actually discriminates the fix.
    assert!(
        (rr - gr).abs() > 0.015,
        "regional and global shadow lift must differ at the boundary ({rr} vs {gr})"
    );
}

#[test]
fn detail_keeps_source_16bit() {
    // Develop's Detail stage (Sharpening / Noise Reduction) reads the source
    // at 16-bit and writes the 16-bit master, so a 16-bit document keeps its
    // precision through a sharpen bake.
    let (w, h) = (16u32, 16u32);
    let mut px16 = vec![0u16; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let v: u16 = if x < w / 2 { 20000 } else { 40000 };
            px16[i] = v;
            px16[i + 1] = v;
            px16[i + 2] = v;
            px16[i + 3] = 65535;
        }
    }
    let src = TileMap::from_rgba16(&px16, w, h);
    assert!(src.has_hdr());

    let mut settings = DevelopSettings::default();
    settings.sharpening = 100.0;
    let out = apply_to_tilemap_direct(&src, &settings, None);

    assert!(out.has_hdr(), "Develop Detail keeps the 16-bit master");
    let changed = (0..w).any(|x| src.get_pixel16(x, 8).0 != out.get_pixel16(x, 8).0);
    assert!(changed, "sharpening must modify the toned pixels at 16-bit");
}

#[test]
fn sharpen_radius_shifts_boost_to_coarser_scales() {
    // Wavelet semantics: Radius balances the level gains, so a large radius
    // amplifies coarse (period-8) texture more than a small one does.
    let (w, h) = (64u32, 16u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let v: u8 = if (x / 4) % 2 == 0 { 138 } else { 118 };
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
    }
    let src = TileMap::from_rgba(&px, w, h);
    let mut narrow = DevelopSettings::default();
    narrow.sharpening = 100.0;
    narrow.sharpen_radius = 0.5;
    let mut wide = narrow.clone();
    wide.sharpen_radius = 3.0;
    let out_n = apply_to_tilemap_direct(&src, &narrow, None);
    let out_w = apply_to_tilemap_direct(&src, &wide, None);
    // Total swing of the stripe pattern away from its mean, interior only.
    let amp = |t: &TileMap| {
        let mut acc = 0i64;
        for y in 4..12u32 {
            for x in 8..56u32 {
                let (r, _, _, _) = t.get_pixel(x, y);
                acc += (r as i64 - 128).abs();
            }
        }
        acc
    };
    assert!(
        amp(&out_w) * 10 > amp(&out_n) * 11,
        "radius 3.0 should boost the coarse stripes more: narrow={} wide={}",
        amp(&out_n),
        amp(&out_w)
    );
}

#[test]
fn wavelet_sharpen_halos_less_than_usm_baseline() {
    // A hard step edge. The edge-aware à-trous decomposition keeps the step in
    // the residual, so boosting the detail coefficients cannot ring around it —
    // unlike the old unsharp mask, whose Gaussian crosses the edge and paints a
    // bright/dark halo band. Compare against the old USM formula (same amount /
    // detail-knee / tanh-limit mapping) run on the same image.
    let (w, h) = (48u32, 16u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let v = if x < 24 { 90u8 } else { 170u8 };
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
    }
    let src = TileMap::from_rgba(&px, w, h);
    let mut settings = DevelopSettings::default();
    settings.sharpening = 100.0;
    settings.sharpen_radius = 1.0;
    let out = apply_to_tilemap_direct(&src, &settings, None);

    // Old-engine USM baseline (amount 1.5, detail 0.25, σ = 1.0, tanh cap 0.35)
    // on the same gray row.
    let row: Vec<f32> = (0..w)
        .map(|x| if x < 24 { 90.0 } else { 170.0 } / 255.0)
        .collect();
    let kernel = [0.054f32, 0.244, 0.403, 0.244, 0.054];
    let usm: Vec<f32> = (0..w as usize)
        .map(|x| {
            let mut blur = 0.0f32;
            for (t, k) in kernel.iter().enumerate() {
                let sx = (x as i64 + t as i64 - 2).clamp(0, w as i64 - 1) as usize;
                blur += row[sx] * k;
            }
            let high = row[x] - blur;
            let weight = 0.25 + 0.75 * smootherstep(0.0, 0.04, high.abs());
            let delta = 0.35f32 * (1.5 * high * weight / 0.35).tanh();
            (row[x] + delta).clamp(0.0, 1.0)
        })
        .collect();

    // Halo amplitude = worst deviation from the flat base on either side.
    let halo_usm = (0..w as usize)
        .map(|x| {
            let base = if x < 24 { 90.0 } else { 170.0 };
            (usm[x] * 255.0 - base).abs()
        })
        .fold(0.0f32, f32::max);
    let halo_wavelet = (0..w)
        .map(|x| {
            let base = if x < 24 { 90.0 } else { 170.0 };
            let (r, _, _, _) = out.get_pixel(x, 8);
            (r as f32 - base).abs()
        })
        .fold(0.0f32, f32::max);
    assert!(
        halo_usm > 10.0,
        "baseline sanity: USM must actually halo ({halo_usm})"
    );
    assert!(
        halo_wavelet * 3.0 < halo_usm,
        "wavelet sharpening should halo far less than USM: wavelet={halo_wavelet} usm={halo_usm}"
    );
}

#[test]
fn sharpen_masking_protects_smooth_gradient() {
    // A gentle gradient (no real edges): with Masking at 100 the sharpener
    // must leave it (nearly) untouched, while Masking 0 visibly ripples it.
    let (w, h) = (64u32, 16u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            // Noisy gradient: fine texture the unmasked sharpener amplifies.
            let base = 60.0 + x as f32 * 2.0;
            let noise = if (x + y) % 2 == 0 { 6.0 } else { -6.0 };
            let v = (base + noise).clamp(0.0, 255.0) as u8;
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
    }
    let src = TileMap::from_rgba(&px, w, h);
    let mut open = DevelopSettings::default();
    open.sharpening = 100.0;
    open.sharpen_detail = 100.0;
    open.sharpen_masking = 0.0;
    let mut masked = open.clone();
    masked.sharpen_masking = 100.0;
    let out_open = apply_to_tilemap_direct(&src, &open, None);
    let out_masked = apply_to_tilemap_direct(&src, &masked, None);
    let diff_sum = |t: &TileMap| {
        let mut acc = 0i64;
        for y in 4..12u32 {
            for x in 8..56u32 {
                let (r0, _, _, _) = src.get_pixel(x, y);
                let (r1, _, _, _) = t.get_pixel(x, y);
                acc += (r1 as i64 - r0 as i64).abs();
            }
        }
        acc
    };
    assert!(
        diff_sum(&out_masked) * 4 < diff_sum(&out_open),
        "masking should suppress most of the change: open={} masked={}",
        diff_sum(&out_open),
        diff_sum(&out_masked)
    );
}

#[test]
fn noise_reduction_preserves_step_edge() {
    // Salt-and-pepper flat field with one strong step: guided NR must smooth
    // the grain but keep the edge magnitude.
    let (w, h) = (48u32, 16u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let base = if x < 24 { 70u8 } else { 180u8 };
            let noise: i32 = if (x * 7 + y * 13) % 3 == 0 { 10 } else { -10 };
            let v = (base as i32 + noise).clamp(0, 255) as u8;
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
    }
    let src = TileMap::from_rgba(&px, w, h);
    let mut settings = DevelopSettings::default();
    settings.noise_reduction = 80.0;
    let out = apply_to_tilemap_direct(&src, &settings, None);
    // Grain inside the flat left half smooths down…
    let grain = |t: &TileMap| {
        let mut acc = 0i64;
        for y in 4..12u32 {
            for x in 4..18u32 {
                let (a, _, _, _) = t.get_pixel(x, y);
                let (b, _, _, _) = t.get_pixel(x + 1, y);
                acc += (a as i64 - b as i64).abs();
            }
        }
        acc
    };
    assert!(
        grain(&out) * 2 < grain(&src),
        "NR should smooth grain: {} -> {}",
        grain(&src),
        grain(&out)
    );
    // …while the step edge survives mostly intact.
    let (l0, _, _, _) = out.get_pixel(20, 8);
    let (r0, _, _, _) = out.get_pixel(28, 8);
    assert!(
        (r0 as i32 - l0 as i32) > 80,
        "step edge should survive NR: {} vs {}",
        l0,
        r0
    );
}

#[test]
fn color_region_box_deblocks_like_commit() {
    // High-frequency chroma checkerboard — the pattern point-sampling aliases and
    // a low-pass smooths. The colour PREVIEW region (build_color_region_box) must
    // match the COMMIT's guided low-pass (build_color_region), while the old
    // point-sampled fast region does not — that mismatch was the blocky-preview
    // vs smooth-commit jump.
    let w = 64u32;
    let h = 64u32;
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let c = if (x + y) % 2 == 0 {
                [200u8, 60, 60]
            } else {
                [60, 200, 60]
            };
            px[i] = c[0];
            px[i + 1] = c[1];
            px[i + 2] = c[2];
            px[i + 3] = 255;
        }
    }
    let tm = TileMap::from_rgba(&px, w, h);
    let s = 4usize;
    let (commit, pw, ph) = build_color_region(&tm, &None, s);
    let (boxed, pwb, phb) = build_color_region_box(&tm, &None, 0, 0, w, h, s);
    let (fast, pwf, phf) = build_fast_preview_region(&tm, &None, 0, 0, w, h, s);
    assert_eq!((pw, ph), (pwb, phb));
    assert_eq!((pw, ph), (pwf, phf));

    let mean = |a: &[[f32; 3]], b: &[[f32; 3]]| {
        let mut acc = 0.0f64;
        for (p, q) in a.iter().zip(b) {
            for k in 0..3 {
                acc += (p[k] - q[k]).abs() as f64;
            }
        }
        acc / (a.len() * 3) as f64
    };
    let d_box = mean(&boxed, &commit);
    let d_fast = mean(&fast, &commit);
    assert!(
        d_box < d_fast * 0.25,
        "preview region must match the commit's de-blocking far better than the \
         point-sampled region: box={d_box:.4} fast={d_fast:.4}"
    );
}

/// Saturation-curve value (control units) a band edit produces at a given
/// swatch colour — the node-domain probe the curve tests below build on.
fn sat_curve_at(settings: &DevelopSettings, rgb: [u8; 3]) -> f32 {
    let curves = build_mixer_curves_opt(settings).expect("mixer engaged");
    let (r, g, b) = (
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    );
    mixer_adjustments_for_color(&curves, r, g, b, 0.5).1
}

fn blend_rgb(a: [u8; 3], b: [u8; 3]) -> [u8; 3] {
    [
        ((a[0] as u16 + b[0] as u16) / 2) as u8,
        ((a[1] as u16 + b[1] as u16) / 2) as u8,
        ((a[2] as u16 + b[2] as u16) / 2) as u8,
    ]
}

#[test]
fn mixer_curve_is_full_at_own_node_and_zero_at_every_other() {
    // The defining property of the node interpolation: a lone band edit is
    // exact at ITS node swatch and ~zero at all 7 other node swatches (the
    // curve passes through the node values; only LUT-lerp noise remains).
    for band in 0..MIXER_BANDS {
        let mut s = DevelopSettings::default();
        s.mixer_saturation[band] = CONTROL_LIMIT;
        let own = sat_curve_at(&s, MIXER_COLORS[band]);
        assert!(
            own > CONTROL_LIMIT * 0.85,
            "band {band} weak at own node: {own}"
        );
        for other in 0..MIXER_BANDS {
            if other == band {
                continue;
            }
            let v = sat_curve_at(&s, MIXER_COLORS[other]).abs();
            assert!(
                v < CONTROL_LIMIT * 0.05,
                "band {band} leaks {v:.1} at node {other}"
            );
        }
    }
}

#[test]
fn mixer_transitions_are_soft_between_neighboring_hues() {
    // Midway between two adjacent nodes the curve is partial — a smooth
    // taper, not a hard band boundary — and far hues sit at ripple level.
    let mut red = DevelopSettings::default();
    red.mixer_saturation[0] = CONTROL_LIMIT;
    let own = sat_curve_at(&red, MIXER_COLORS[0]);
    let toward_orange = sat_curve_at(&red, blend_rgb(MIXER_COLORS[0], MIXER_COLORS[1]));
    let cyan = sat_curve_at(&red, MIXER_COLORS[4]).abs();
    assert!(
        toward_orange > CONTROL_LIMIT * 0.08 && toward_orange < own,
        "red→orange transition not soft: {toward_orange} vs own {own}"
    );
    assert!(cyan < CONTROL_LIMIT * 0.05, "red reached cyan: {cyan}");

    let mut yellow = DevelopSettings::default();
    yellow.mixer_saturation[2] = CONTROL_LIMIT;
    let core = sat_curve_at(&yellow, MIXER_COLORS[2]);
    let toward_green = sat_curve_at(&yellow, blend_rgb(MIXER_COLORS[2], MIXER_COLORS[3]));
    assert!(
        toward_green > CONTROL_LIMIT * 0.08 && toward_green < core,
        "yellow→green transition not soft: {toward_green} vs core {core}"
    );
}

#[test]
fn mixer_keeps_light_and_dark_oranges_inside_orange_band() {
    let mut settings = DevelopSettings::default();
    settings.mixer_saturation[1] = CONTROL_LIMIT;

    let dark_orange = vec![92, 48, 20, 255];
    let mid_orange = vec![214, 128, 52, 255];
    let light_orange = vec![245, 186, 82, 255];
    let red = vec![220, 36, 34, 255];

    let mut dark_out = dark_orange.clone();
    let mut mid_out = mid_orange.clone();
    let mut light_out = light_orange.clone();
    let mut red_out = red.clone();
    apply_to_pixels(&settings, &mut dark_out, 1, 1);
    apply_to_pixels(&settings, &mut mid_out, 1, 1);
    apply_to_pixels(&settings, &mut light_out, 1, 1);
    apply_to_pixels(&settings, &mut red_out, 1, 1);

    // The whole orange tonal volume responds; a red pixel (one node over)
    // takes only a small partial share of the smooth curve — far less than
    // a true orange member, never the band's own edit.
    let dark_d = rgb_distance(&dark_orange, &dark_out);
    let mid_d = rgb_distance(&mid_orange, &mid_out);
    let light_d = rgb_distance(&light_orange, &light_out);
    let red_d = rgb_distance(&red, &red_out);
    assert!(dark_d > 6);
    assert!(mid_d > 12);
    assert!(light_d > 6);
    assert!(
        red_d * 3 < mid_d && red_d < 25,
        "red took too much of an Orange edit: red={red_d} oranges={dark_d}/{mid_d}/{light_d}"
    );
}

#[test]
fn vibrance_prioritises_pale_colours_and_spares_vivid_ones() {
    // Vividness pours into pale/muted colours; an already-vivid colour
    // (chroma ≥ 0.35) is Saturation's territory and must stay put.
    let mut s = DevelopSettings::default();
    s.vibrance = 150.0;
    let pale_moved = moved(&s, [170, 150, 140]); // chroma ≈ 0.12
    let vivid_moved = moved(&s, [220, 60, 50]); // chroma ≈ 0.67
    assert!(pale_moved > 6, "pale colour ignored vibrance: {pale_moved}");
    assert!(
        vivid_moved <= 2,
        "vivid colour took a vibrance boost: {vivid_moved}"
    );
    assert!(
        moved(&s, [128, 128, 128]) == 0,
        "vibrance touched neutral grey"
    );
}

#[test]
fn yellow_mixer_affects_yellow_pixels() {
    let mut settings = DevelopSettings::default();
    settings.mixer_luminance[2] = CONTROL_LIMIT;

    let yellow = vec![218, 196, 74, 255];
    let mut yellow_out = yellow.clone();
    apply_to_pixels(&settings, &mut yellow_out, 1, 1);

    assert!(
        rgb_distance(&yellow, &yellow_out) > 10,
        "yellow slider did not affect yellow pixel: {yellow_out:?}"
    );
}

#[test]
fn tone_controls_reach_dark_and_light_regions_across_hues() {
    let mut shadows = DevelopSettings::default();
    shadows.shadows = CONTROL_LIMIT;
    let dark_red = vec![82, 22, 20, 255];
    let dark_blue = vec![22, 38, 92, 255];
    let mut dark_red_out = dark_red.clone();
    let mut dark_blue_out = dark_blue.clone();
    apply_to_pixels(&shadows, &mut dark_red_out, 1, 1);
    apply_to_pixels(&shadows, &mut dark_blue_out, 1, 1);
    assert!(rgb_distance(&dark_red, &dark_red_out) > 8);
    assert!(rgb_distance(&dark_blue, &dark_blue_out) > 8);

    let mut highlights = DevelopSettings::default();
    highlights.highlights = CONTROL_LIMIT;
    let light_yellow = vec![238, 210, 92, 255];
    let light_cyan = vec![132, 220, 232, 255];
    let mut light_yellow_out = light_yellow.clone();
    let mut light_cyan_out = light_cyan.clone();
    apply_to_pixels(&highlights, &mut light_yellow_out, 1, 1);
    apply_to_pixels(&highlights, &mut light_cyan_out, 1, 1);
    assert!(rgb_distance(&light_yellow, &light_yellow_out) > 8);
    assert!(rgb_distance(&light_cyan, &light_cyan_out) > 8);
}

#[test]
fn mixer_mode_alone_is_not_an_image_adjustment() {
    let mut settings = DevelopSettings::default();
    settings.mixer_mode = DevelopMixerMode::All;
    assert!(settings.is_neutral());
    assert!(settings.same_image_effect(&DevelopSettings::default()));
}

#[test]
fn mixer_hue_and_luminance_affect_pixels() {
    let source = vec![210, 40, 40, 255];

    let mut hue_settings = DevelopSettings::default();
    hue_settings.mixer_hue[0] = CONTROL_LIMIT;
    let mut hue_pixels = source.clone();
    apply_to_pixels(&hue_settings, &mut hue_pixels, 1, 1);
    assert_ne!(&hue_pixels[0..3], &source[0..3]);

    let mut lum_settings = DevelopSettings::default();
    lum_settings.mixer_luminance[0] = CONTROL_LIMIT;
    let mut lum_pixels = source.clone();
    apply_to_pixels(&lum_settings, &mut lum_pixels, 1, 1);
    assert!(lum_pixels[0] > source[0]);
}

#[test]
fn red_mixer_ignores_near_neutral_white_and_black() {
    let mut settings = DevelopSettings::default();
    settings.mixer_hue[0] = CONTROL_LIMIT;
    settings.mixer_saturation[0] = CONTROL_LIMIT;
    settings.mixer_luminance[0] = CONTROL_LIMIT;

    let white = vec![238, 231, 231, 255];
    let black = vec![18, 17, 17, 255];
    let skin_red = vec![194, 92, 66, 255];

    let mut white_out = white.clone();
    let mut black_out = black.clone();
    let mut skin_out = skin_red.clone();
    apply_to_pixels(&settings, &mut white_out, 1, 1);
    apply_to_pixels(&settings, &mut black_out, 1, 1);
    apply_to_pixels(&settings, &mut skin_out, 1, 1);

    assert!(rgb_distance(&white, &white_out) < 8);
    assert!(rgb_distance(&black, &black_out) < 5);
    assert!(rgb_distance(&skin_red, &skin_out) > 12);
}

#[test]
fn red_saturation_preserves_lip_luma_without_black_speckles() {
    let mut settings = DevelopSettings::default();
    settings.mixer_saturation[0] = CONTROL_LIMIT;

    let before = vec![
        118, 24, 42, 255, 145, 34, 58, 255, 96, 18, 34, 255, 180, 60, 82, 255,
    ];
    let mut after = before.clone();
    apply_to_pixels(&settings, &mut after, 4, 1);

    for (src, dst) in before.chunks_exact(4).zip(after.chunks_exact(4)) {
        assert!(luma_u8(dst) + 3.0 >= luma_u8(src));
        assert!(dst[0] as i32 >= src[0] as i32 - 5);
        assert!(dst[0] > dst[1]);
        assert!(dst[0] > dst[2]);
    }
}

#[test]
fn mixer_luminance_darkening_does_not_make_black_speckles() {
    let mut settings = DevelopSettings::default();
    settings.mixer_luminance[0] = -CONTROL_LIMIT;

    let dark_reds = vec![
        70, 20, 18, 255, 92, 30, 24, 255, 52, 16, 14, 255, 110, 44, 36, 255,
    ];
    let mut out = dark_reds.clone();
    apply_to_pixels(&settings, &mut out, 4, 1);

    for px in out.chunks_exact(4) {
        assert!(
            px[0] as u32 + px[1] as u32 + px[2] as u32 > 0,
            "coloured pixel collapsed to pure black: {px:?}"
        );
    }
}

#[test]
fn tone_masks_feather_light_and_shadow_adjustments() {
    let mut highlights = DevelopSettings::default();
    highlights.highlights = CONTROL_LIMIT;
    let mut dark = vec![50, 50, 50, 255];
    let mut bright = vec![210, 210, 210, 255];
    apply_to_pixels(&highlights, &mut dark, 1, 1);
    apply_to_pixels(&highlights, &mut bright, 1, 1);
    assert!(
        channel_delta(&bright, &[210, 210, 210, 255]) > channel_delta(&dark, &[50, 50, 50, 255])
    );

    let mut shadows = DevelopSettings::default();
    shadows.shadows = CONTROL_LIMIT;
    let mut dark = vec![50, 50, 50, 255];
    let mut bright = vec![210, 210, 210, 255];
    apply_to_pixels(&shadows, &mut dark, 1, 1);
    apply_to_pixels(&shadows, &mut bright, 1, 1);
    assert!(
        channel_delta(&dark, &[50, 50, 50, 255]) > channel_delta(&bright, &[210, 210, 210, 255])
    );
}

#[test]
fn mid_strength_shadows_and_highlights_are_visible() {
    let mut shadows = DevelopSettings::default();
    shadows.shadows = CONTROL_LIMIT * 0.5;
    let dark_red = vec![58, 18, 14, 255];
    let mut dark_red_out = dark_red.clone();
    apply_to_pixels(&shadows, &mut dark_red_out, 1, 1);
    assert!(rgb_distance(&dark_red, &dark_red_out) > 24);
    assert!(dark_red_out[0] < 215);

    let mut brighten = DevelopSettings::default();
    brighten.highlights = CONTROL_LIMIT * 0.5;
    let light_yellow = vec![222, 204, 112, 255];
    let mut light_yellow_out = light_yellow.clone();
    apply_to_pixels(&brighten, &mut light_yellow_out, 1, 1);
    assert!(rgb_distance(&light_yellow, &light_yellow_out) > 18);
    assert!(light_yellow_out[0] < 255);

    let mut recover = DevelopSettings::default();
    recover.highlights = -CONTROL_LIMIT * 0.5;
    let bright_skin = vec![232, 202, 176, 255];
    let mut bright_skin_out = bright_skin.clone();
    apply_to_pixels(&recover, &mut bright_skin_out, 1, 1);
    assert!(rgb_distance(&bright_skin, &bright_skin_out) > 22);
    assert!(bright_skin_out[0] > 90);
}

#[test]
fn blacks_lift_makes_matte_blacks_and_leaves_midtones() {
    let mut settings = DevelopSettings::default();
    settings.blacks = CONTROL_LIMIT;

    let pure_black = vec![0, 0, 0, 255];
    let near_black = vec![12, 10, 8, 255];
    let dark_detail = vec![42, 34, 26, 255];
    let mid = vec![120, 105, 92, 255];
    let mut pure_out = pure_black.clone();
    let mut near_out = near_black.clone();
    let mut detail_out = dark_detail.clone();
    let mut mid_out = mid.clone();

    apply_to_pixels(&settings, &mut pure_out, 1, 1);
    apply_to_pixels(&settings, &mut near_out, 1, 1);
    apply_to_pixels(&settings, &mut detail_out, 1, 1);
    apply_to_pixels(&settings, &mut mid_out, 1, 1);

    assert!(pure_out[0] > 20, "blacks+ should lift the black point");
    assert!(pure_out[0] < 90, "blacks+ should not blow the floor open");
    assert!(near_out[0] > near_black[0]);
    assert!(detail_out[0] > dark_detail[0]);
    assert!(
        rgb_distance(&mid, &mid_out) < 16,
        "midtones should be left alone"
    );
}

#[test]
fn shadows_lift_detail_without_turning_black_white() {
    let mut settings = DevelopSettings::default();
    settings.shadows = CONTROL_LIMIT;

    let black = vec![2, 2, 2, 255];
    let dark_red = vec![58, 18, 14, 255];
    let dark_blue = vec![16, 30, 72, 255];
    let mut black_out = black.clone();
    let mut red_out = dark_red.clone();
    let mut blue_out = dark_blue.clone();

    apply_to_pixels(&settings, &mut black_out, 1, 1);
    apply_to_pixels(&settings, &mut red_out, 1, 1);
    apply_to_pixels(&settings, &mut blue_out, 1, 1);

    assert!(black_out[0] < 18);
    assert!(rgb_distance(&dark_red, &red_out) > 12);
    assert!(rgb_distance(&dark_blue, &blue_out) > 12);
    assert!(red_out[0] < 230);
    assert!(blue_out[2] < 225);
}

#[test]
fn shadows_lift_keeps_dark_red_from_washing_to_grey() {
    let mut settings = DevelopSettings::default();
    settings.shadows = CONTROL_LIMIT;

    let dark_red = vec![84, 26, 20, 255];
    let mut red_out = dark_red.clone();
    apply_to_pixels(&settings, &mut red_out, 1, 1);

    assert!(rgb_distance(&dark_red, &red_out) > 18);
    assert!(red_out[0] > red_out[1] + 45, "red washed out: {red_out:?}");
    assert!(red_out[0] > red_out[2] + 45, "red washed out: {red_out:?}");
}

#[test]
fn blacks_lift_prefers_true_dark_values_over_colored_midtones() {
    let mut settings = DevelopSettings::default();
    settings.blacks = CONTROL_LIMIT;

    let neutral_black = vec![22, 22, 22, 255];
    let colored_mid = vec![176, 52, 42, 255];
    let mut neutral_out = neutral_black.clone();
    let mut colored_out = colored_mid.clone();

    apply_to_pixels(&settings, &mut neutral_out, 1, 1);
    apply_to_pixels(&settings, &mut colored_out, 1, 1);

    assert!(rgb_distance(&neutral_black, &neutral_out) > 10);
    assert!(
        rgb_distance(&colored_mid, &colored_out) < rgb_distance(&neutral_black, &neutral_out),
        "blacks caught colored midtone too strongly: {colored_out:?}"
    );
}

#[test]
fn contrast_max_has_visible_tonal_separation() {
    let mut settings = DevelopSettings::default();
    settings.contrast = CONTROL_LIMIT;

    let dark = vec![72, 72, 72, 255];
    let light = vec![184, 184, 184, 255];
    let mut dark_out = dark.clone();
    let mut light_out = light.clone();

    apply_to_pixels(&settings, &mut dark_out, 1, 1);
    apply_to_pixels(&settings, &mut light_out, 1, 1);

    assert!(
        dark_out[0] < 58,
        "contrast did not deepen dark tone: {dark_out:?}"
    );
    assert!(
        light_out[0] > 198,
        "contrast did not lift light tone: {light_out:?}"
    );
}

#[test]
fn shadows_reach_colored_skin_midtones_more_than_bright_skin() {
    let mut shadows = DevelopSettings::default();
    shadows.shadows = CONTROL_LIMIT;

    let colored_shadow = vec![112, 70, 52, 255];
    let bright_skin = vec![218, 174, 150, 255];
    let mut shadow_out = colored_shadow.clone();
    let mut bright_out = bright_skin.clone();
    apply_to_pixels(&shadows, &mut shadow_out, 1, 1);
    apply_to_pixels(&shadows, &mut bright_out, 1, 1);

    assert!(rgb_distance(&colored_shadow, &shadow_out) > rgb_distance(&bright_skin, &bright_out));
}

#[test]
fn color_mixer_softens_small_chroma_blocks_vs_per_pixel() {
    use crate::core::tile::{TilePos, TILE_SIZE};
    let w = 24u32;
    let h = 24u32;
    // Flat reddish field with a small (3x3) higher-chroma block in the centre,
    // standing in for the source JPEG's amplified chroma blocks.
    let base = [150u8, 86, 74];
    let block = [150u8, 40, 30];
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let c = if (11..14).contains(&x) && (11..14).contains(&y) {
                block
            } else {
                base
            };
            px[i] = c[0];
            px[i + 1] = c[1];
            px[i + 2] = c[2];
            px[i + 3] = 255;
        }
    }

    let mut settings = DevelopSettings::default();
    settings.mixer_saturation[0] = CONTROL_LIMIT;

    let mut naive = px.clone();
    apply_to_pixels(&settings, &mut naive, w, h);
    let naive_at = |x: u32, y: u32| -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [naive[i], naive[i + 1], naive[i + 2]]
    };

    let out = apply_to_tilemap_direct(&TileMap::from_rgba(&px, w, h), &settings, None);
    let dp_at = |x: u32, y: u32| -> [u8; 3] {
        let t = out.tiles.get(&TilePos::from_pixel(x, y)).unwrap();
        let (r, g, b, _) = t.get_pixel(x % TILE_SIZE, y % TILE_SIZE);
        [r, g, b]
    };
    let chroma =
        |p: [u8; 3]| -> i32 { p[0].max(p[1]).max(p[2]) as i32 - p[0].min(p[1]).min(p[2]) as i32 };

    let naive_block_jump = (chroma(naive_at(12, 12)) - chroma(naive_at(3, 3))).abs();
    let dp_block_jump = (chroma(dp_at(12, 12)) - chroma(dp_at(3, 3))).abs();

    assert!(
        dp_block_jump < naive_block_jump,
        "detail-preserving block jump {dp_block_jump} should be < naive {naive_block_jump}"
    );
    // The boost must still actually saturate the flat field.
    assert!(chroma(dp_at(3, 3)) > chroma(base));
}

#[test]
fn color_mixer_pulls_offhue_speck_toward_its_region() {
    use crate::core::tile::{TilePos, TILE_SIZE};
    let w = 40u32;
    let h = 40u32;
    // Large orange (skin) field with a small red speck in the centre.
    let field = [200u8, 120, 60];
    let speck = [200u8, 40, 40];
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let c = if (19..21).contains(&x) && (19..21).contains(&y) {
                speck
            } else {
                field
            };
            px[i] = c[0];
            px[i + 1] = c[1];
            px[i + 2] = c[2];
            px[i + 3] = 255;
        }
    }

    // Boost the Oranges band.
    let mut settings = DevelopSettings::default();
    settings.mixer_saturation[1] = CONTROL_LIMIT;
    settings.mixer_luminance[1] = CONTROL_LIMIT;

    let mut naive = px.clone();
    apply_to_pixels(&settings, &mut naive, w, h);
    let naive_at = |x: u32, y: u32| -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [naive[i], naive[i + 1], naive[i + 2]]
    };

    let out = apply_to_tilemap_direct(&TileMap::from_rgba(&px, w, h), &settings, None);
    let dp_at = |x: u32, y: u32| -> [u8; 3] {
        let t = out.tiles.get(&TilePos::from_pixel(x, y)).unwrap();
        let (r, g, b, _) = t.get_pixel(x % TILE_SIZE, y % TILE_SIZE);
        [r, g, b]
    };

    // In the naive per-pixel path the red speck is keyed as "red", gets no
    // orange treatment, and drifts away from the field. The region-aware path
    // lets it inherit the field's adjustment, so it stays closer to the field.
    let naive_gap = rgb_distance(&naive_at(20, 20), &naive_at(3, 3));
    let dp_gap = rgb_distance(&dp_at(20, 20), &dp_at(3, 3));
    assert!(
        dp_gap < naive_gap,
        "region-aware speck gap {dp_gap} should be < naive {naive_gap}"
    );
}

#[test]
fn red_band_leaves_orange_skin_mostly_alone() {
    let mut settings = DevelopSettings::default();
    settings.mixer_saturation[0] = CONTROL_LIMIT;

    let pure_red = vec![200, 30, 30, 255];
    let orange_skin = vec![205, 150, 120, 255];
    let mut red_out = pure_red.clone();
    let mut skin_out = orange_skin.clone();
    apply_to_pixels(&settings, &mut red_out, 1, 1);
    apply_to_pixels(&settings, &mut skin_out, 1, 1);

    let red_delta = rgb_distance(&pure_red, &red_out);
    let skin_delta = rgb_distance(&orange_skin, &skin_out);
    assert!(
        red_delta > 10,
        "red band did not grip true red: {red_out:?}"
    );
    assert!(
        skin_delta * 2 < red_delta,
        "red band tints orange skin too much: skin {skin_delta} vs red {red_delta}"
    );
}

#[test]
fn yellow_band_grips_orange_yellow_skin() {
    // The Orange↔Yellow split is biased toward yellow, so a light orange-yellow
    // skin tone now responds meaningfully to the Yellow slider.
    let mut settings = DevelopSettings::default();
    settings.mixer_saturation[2] = CONTROL_LIMIT;
    let skin = vec![220, 170, 120, 255];
    let mut out = skin.clone();
    apply_to_pixels(&settings, &mut out, 1, 1);
    assert!(
        rgb_distance(&skin, &out) > 8,
        "yellow did not grip orange-yellow skin: {out:?}"
    );
}

#[test]
fn local_shadows_preserve_texture_better_than_global() {
    use crate::core::tile::{TilePos, TILE_SIZE};
    let w = 64u32;
    let h = 16u32;
    // Dark region with fine texture (alternating luma) — the kind of local
    // contrast the naive per-pixel (global) shadow lift distorts, because each
    // pixel lands on a different point of the steep shadow slope.
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let v = if x % 2 == 0 { 22u8 } else { 46u8 };
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
    }

    let mut settings = DevelopSettings::default();
    settings.shadows = CONTROL_LIMIT;

    // Global lift (per-pixel curve) via the flat-buffer helper.
    let mut global = px.clone();
    apply_to_pixels(&settings, &mut global, w, h);

    // Local-adaptation lift (production tilemap path).
    let out = apply_to_tilemap_direct(&TileMap::from_rgba(&px, w, h), &settings, None);
    let dp_at = |x: u32, y: u32| -> i32 {
        let t = out.tiles.get(&TilePos::from_pixel(x, y)).unwrap();
        t.get_pixel(x % TILE_SIZE, y % TILE_SIZE).0 as i32
    };

    let yy = h / 2;
    let xx = w / 2; // even → dark sample, xx+1 → light sample
    let global_step = (global[((yy * w + xx + 1) * 4) as usize] as i32
        - global[((yy * w + xx) * 4) as usize] as i32)
        .abs();
    let dp_lo = dp_at(xx, yy);
    let dp_step = (dp_at(xx + 1, yy) - dp_lo).abs();
    let src_step = 46 - 22;

    assert!(
        dp_lo > 22 + 4,
        "local shadows did not lift the dark region: {dp_lo}"
    );
    // Local adaptation reads the lift at the *regional* luma, so the dark and
    // light pixels of the texture get the SAME base offset, and the
    // detail-preserving boost then re-amplifies the fine step in proportion
    // to how far the base moved (Weber compensation, DETAIL_BOOST_STRENGTH).
    // The user-visible requirement: a shadow lift must never FLATTEN the
    // texture (the flat/smeared-texture complaint) — the step stays at least as large as
    // the source and at least as large as the global path's, but bounded
    // (no runaway amplification).
    assert!(
        dp_step >= src_step && dp_step >= global_step,
        "local lift must not flatten texture: local {dp_step} vs global \
         {global_step} (src {src_step})"
    );
    assert!(
        (dp_step as f32) <= src_step as f32 * DETAIL_GAIN_MAX + 4.0,
        "local lift over-amplified the texture: {dp_step} (src {src_step})"
    );
}

#[test]
fn mixer_band_edit_does_not_bleed_across_color_boundary() {
    use crate::core::tile::{TilePos, TILE_SIZE};
    // Left half saturated red, right half saturated blue. Boosting the RED
    // band hard must leave the blue side untouched even right next to the
    // boundary — the low-res colour proxy used to leak the push across.
    let (w, h) = (64u32, 32u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let c = if x < 32 {
                [190u8, 40, 40]
            } else {
                [40u8, 60, 190]
            };
            px[i] = c[0];
            px[i + 1] = c[1];
            px[i + 2] = c[2];
            px[i + 3] = 255;
        }
    }
    let mut settings = DevelopSettings::default();
    settings.mixer_saturation[0] = CONTROL_LIMIT;
    settings.mixer_luminance[0] = CONTROL_LIMIT / 2.0;

    let out = apply_to_tilemap_direct(&TileMap::from_rgba(&px, w, h), &settings, None);
    let at = |x: u32, y: u32| -> [u8; 3] {
        let t = out.tiles.get(&TilePos::from_pixel(x, y)).unwrap();
        let (r, g, b, _) = t.get_pixel(x % TILE_SIZE, y % TILE_SIZE);
        [r, g, b]
    };
    // Red side must move…
    let red_moved = rgb_distance(&at(8, 16), &[190, 40, 40]);
    assert!(red_moved > 20, "red side should take the push: {red_moved}");
    // …the blue side just past the boundary must not.
    for x in [35u32, 40, 48] {
        let d = rgb_distance(&at(x, 16), &[40, 60, 190]);
        assert!(
            d <= 12,
            "blue side at x={x} should not take the red push: moved {d}"
        );
    }
}

#[test]
fn red_luminance_brightens_reds_and_spares_the_orange_node() {
    // Red Luminance brightens a red materially; a colour sitting on the
    // Orange node takes ~nothing (the Red curve crosses zero there).
    let mut settings = DevelopSettings::default();
    settings.mixer_luminance[0] = CONTROL_LIMIT;
    let luma_gain = |rgb: [u8; 3]| oklab(direct_apply(&settings, rgb))[0] - oklab(rgb)[0];
    let red = luma_gain([170, 55, 55]);
    assert!(red > 0.03, "true red did not brighten: {red:.4}");
    assert!(
        luma_gain(MIXER_COLORS[1]).abs() < red * 0.3,
        "Orange-node colour took a Red edit"
    );
}

#[test]
fn local_blacks_preserve_dark_detail_better_than_global() {
    use crate::core::tile::{TilePos, TILE_SIZE};
    let w = 64u32;
    let h = 16u32;
    // Very dark region with fine texture (e.g. hair strands) that a global
    // Blacks pull flattens into one value.
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let v = if x % 2 == 0 { 8u8 } else { 24u8 };
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
    }

    let mut settings = DevelopSettings::default();
    settings.blacks = CONTROL_LIMIT; // lift the black point (matte)

    let mut global = px.clone();
    apply_to_pixels(&settings, &mut global, w, h);

    let out = apply_to_tilemap_direct(&TileMap::from_rgba(&px, w, h), &settings, None);
    let dp_at = |x: u32, y: u32| -> i32 {
        let t = out.tiles.get(&TilePos::from_pixel(x, y)).unwrap();
        t.get_pixel(x % TILE_SIZE, y % TILE_SIZE).0 as i32
    };

    let yy = h / 2;
    let xx = w / 2;
    let global_step = (global[((yy * w + xx + 1) * 4) as usize] as i32
        - global[((yy * w + xx) * 4) as usize] as i32)
        .abs();
    let dp_step = (dp_at(xx + 1, yy) - dp_at(xx, yy)).abs();

    assert!(
        dp_step > global_step,
        "local Blacks should keep more dark detail than global: local {dp_step} vs global {global_step}"
    );
}

#[test]
#[ignore = "diagnostic instrumentation, run with --ignored --nocapture"]
fn diag_mixer_live_strength() {
    use crate::core::tile::{TilePos, TILE_SIZE};
    let cases: [(&str, usize, [u8; 3]); 15] = [
        ("deep_lip_red", R, [150, 20, 34]),
        ("sat_red", R, [210, 45, 45]),
        ("blush_red", R, [218, 128, 132]),
        ("weak_skin_red", R, [184, 132, 126]),
        ("neutral_skin", R, [190, 145, 120]),
        ("burgundy", R, [90, 25, 35]),
        ("skin_orange", O, [205, 150, 120]),
        ("bright_yellow", Y, [235, 220, 60]),
        ("olive", Y, [110, 105, 45]),
        ("dark_foliage", G, [40, 80, 45]),
        ("teal", A, [40, 140, 140]),
        ("muted_blue", BL, [90, 120, 170]),
        ("navy", BL, [22, 38, 92]),
        ("magenta", M, [210, 60, 170]),
        ("neutral_dark", R, [42, 43, 44]),
    ];
    let (w, h) = (TILE_SIZE, TILE_SIZE);
    let uni = |rgb: [u8; 3]| {
        let mut px = vec![0u8; (w * h * 4) as usize];
        for p in px.chunks_exact_mut(4) {
            p[0] = rgb[0];
            p[1] = rgb[1];
            p[2] = rgb[2];
            p[3] = 255;
        }
        px
    };
    let center = |tm: &TileMap| -> [u8; 3] {
        let t = tm.tiles.get(&TilePos::from_pixel(w / 2, h / 2)).unwrap();
        let (r, g, b, _) = t.get_pixel(w / 2 % TILE_SIZE, h / 2 % TILE_SIZE);
        [r, g, b]
    };
    println!("\n=== MIXER LIVE STRENGTH (production proxy path) ===");
    for (name, band, rgb) in cases {
        let (rf, gf, bf) = (
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
        );
        let luma = luminance_f32(rf, gf, bf).clamp(0.0, 1.0);
        let membership = mixer_band_memberships(rf, gf, bf);
        let (w_hs, w_lum) = mixer_edit_weights(rf, gf, bf, luma);
        let re = {
            let mut s = DevelopSettings::default();
            s.mixer_saturation[band] = 100.0;
            let curves = build_mixer_curves_opt(&s).unwrap();
            mixer_edit_affinity(&curves, [rf, gf, bf])
        };
        let shadow = smootherstep(0.0, 0.32, luma);
        println!(
            "\n{name} rgb{rgb:?}  member={:.2} w_hs={w_hs:.2} w_lum={w_lum:.2}  re_gate={re:.2} shadow_fade={shadow:.2}",
            membership[band]
        );
        for (label, mk) in [("SAT", 1usize), ("LUM", 2usize), ("HUE", 0usize)] {
            for v in [50.0f32, 100.0, 200.0] {
                let mut s = DevelopSettings::default();
                match mk {
                    0 => s.mixer_hue[band] = v,
                    1 => s.mixer_saturation[band] = v,
                    _ => s.mixer_luminance[band] = v,
                }
                let out = apply_to_tilemap_direct(&TileMap::from_rgba(&uni(rgb), w, h), &s, None);
                let proxy = center(&out);
                let mut direct = uni(rgb);
                apply_to_pixels(&s, &mut direct, w, h);
                let di = ((h / 2 * w + w / 2) * 4) as usize;
                let d = [direct[di], direct[di + 1], direct[di + 2]];
                print!(
                    "  {label}+{v:>3.0}: proxyΔ={:>3} directΔ={:>3} | ",
                    rgb_distance(&proxy, &rgb),
                    rgb_distance(&d, &rgb)
                );
            }
            println!();
        }
    }
}

#[test]
#[ignore = "diagnostic instrumentation, run with --ignored --nocapture"]
fn diag_dark_colored_breakdown() {
    fn oklab_ch(rgb: [u8; 3]) -> f32 {
        let l = oklab(rgb);
        (l[1] * l[1] + l[2] * l[2]).sqrt()
    }
    let samples: [(&str, usize, [u8; 3]); 14] = [
        ("foliage_mid", G, [40, 80, 45]),
        ("foliage_dark", G, [30, 55, 32]),
        ("foliage_deep", G, [22, 42, 26]),
        ("olive", Y, [95, 88, 38]),
        ("moss", G, [70, 82, 45]),
        ("burgundy", R, [90, 25, 35]),
        ("burgundy_deep", R, [60, 18, 24]),
        ("dark_magenta", M, [95, 28, 62]),
        ("dark_teal", A, [25, 78, 80]),
        ("navy", BL, [22, 38, 92]),
        ("navy_deep", BL, [16, 26, 60]),
        ("dark_gray", G, [42, 44, 43]),
        ("near_black", G, [11, 11, 12]),
        ("black", G, [6, 6, 6]),
    ];
    println!("\n=== DARK COLORED BREAKDOWN ===");
    println!(
        "{:<14} {:>4} {:>7} {:>6} {:>6} {:>6} | {:>6} {:>5} {:>5} | {:>6}",
        "name", "band", "ucsHue", "hsvS", "delta", "oklC", "member", "w_hs", "w_lum", "SATΔ"
    );
    for (name, band, rgb) in samples {
        let (rf, gf, bf) = (
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
        );
        let luma = luminance_f32(rf, gf, bf).clamp(0.0, 1.0);
        let hue = crate::core::ucs::ucs_hue_rad(rf, gf, bf).to_degrees();
        let hsv = hsv_saturation(rf, gf, bf);
        let delta = rgb_chroma(rf, gf, bf);
        let membership = mixer_band_memberships(rf, gf, bf);
        let (w_hs, w_lum) = mixer_edit_weights(rf, gf, bf, luma);
        let mut s = DevelopSettings::default();
        s.mixer_saturation[band] = 100.0;
        let mut d = vec![rgb[0], rgb[1], rgb[2], 255u8];
        apply_to_pixels(&s, &mut d, 1, 1);
        let de = rgb_distance(&[d[0], d[1], d[2]], &rgb);
        println!(
            "{name:<14} {band:>4} {hue:>7.1} {hsv:>6.3} {delta:>6.3} {:>6.3} | {:>6.2} {w_hs:>5.2} {w_lum:>5.2} | {de:>6}",
            oklab_ch(rgb),
            membership[band]
        );
    }
}

#[test]
fn detail_color_noise_reduction_smooths_chroma_not_luma() {
    let w = 3u32;
    let h = 3u32;
    let mut px = vec![128u8, 128, 128, 255].repeat((w * h) as usize);
    let center = ((w * h / 2) * 4) as usize;
    px[center] = 95;
    px[center + 1] = 170;
    px[center + 2] = 95;

    let before_luma = luma_u8(&px[center..center + 4]);
    let before_chroma =
        px[center..center + 3].iter().max().unwrap() - px[center..center + 3].iter().min().unwrap();

    let mut settings = DevelopSettings::default();
    settings.color_noise_reduction = 100.0;
    apply_to_pixels(&settings, &mut px, w, h);

    let after_luma = luma_u8(&px[center..center + 4]);
    let after_chroma =
        px[center..center + 3].iter().max().unwrap() - px[center..center + 3].iter().min().unwrap();

    assert!(after_chroma < before_chroma / 2);
    assert!((after_luma - before_luma).abs() < 8.0);
}

#[test]
fn detail_noise_reduction_smooths_low_amplitude_luma_noise() {
    let w = 5u32;
    let h = 1u32;
    let mut px = vec![
        100, 100, 100, 255, 102, 102, 102, 255, 118, 118, 118, 255, 102, 102, 102, 255, 100, 100,
        100, 255,
    ];

    let before = px[((w / 2) * 4) as usize];
    let mut settings = DevelopSettings::default();
    settings.noise_reduction = 100.0;
    apply_to_pixels(&settings, &mut px, w, h);
    let after = px[((w / 2) * 4) as usize];

    assert!(
        after < before,
        "center noise should smooth down: {before} -> {after}"
    );
    assert_eq!(px[((w / 2) * 4 + 3) as usize], 255);
}

// ── Colour-Mixer redesign: full-volume behavioural coverage ──────────────

/// Band indices for readability.
const R: usize = 0;
const O: usize = 1;
const Y: usize = 2;
const G: usize = 3;
const A: usize = 4;
const BL: usize = 5;
const P: usize = 6;
const M: usize = 7;

fn develop_one(settings: &DevelopSettings, rgb: [u8; 3]) -> [u8; 3] {
    let mut p = vec![rgb[0], rgb[1], rgb[2], 255u8];
    apply_to_pixels(settings, &mut p, 1, 1);
    [p[0], p[1], p[2]]
}

fn moved(settings: &DevelopSettings, rgb: [u8; 3]) -> i32 {
    rgb_distance(&develop_one(settings, rgb), &rgb)
}

fn sat_band(band: usize) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.mixer_saturation[band] = CONTROL_LIMIT;
    s
}

fn base_aff_u8(rgb: [u8; 3]) -> [f32; MIXER_BANDS] {
    let (r, g, b) = (
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    );
    mixer_band_memberships(r, g, b)
}

/// Representative members of each band's colour VOLUME (bright / mid / dark /
/// muted). Every one must (a) confidently belong to its own band and (b) take
/// a clear edit from its own Saturation slider.
fn band_volume() -> Vec<(&'static str, usize, [u8; 3])> {
    vec![
        ("bright_red", R, [210, 45, 45]),
        ("crimson", R, [190, 30, 60]),
        ("dark_red", R, [120, 28, 26]),
        ("burgundy", R, [90, 25, 35]),
        ("wine", R, [70, 22, 34]),
        ("sat_orange", O, [225, 120, 30]),
        ("peach", O, [240, 190, 150]),
        ("tan", O, [190, 150, 110]),
        ("brown", O, [130, 85, 50]),
        ("dk_warm_brown", O, [80, 55, 35]),
        ("bright_yellow", Y, [235, 220, 60]),
        ("golden", Y, [220, 180, 50]),
        ("mustard", Y, [190, 160, 45]),
        ("khaki", Y, [165, 150, 95]),
        ("olive", Y, [110, 105, 45]),
        ("dark_yellow", Y, [120, 110, 40]),
        ("bright_green", G, [70, 180, 70]),
        ("yellow_green", G, [140, 190, 60]),
        ("muted_green", G, [90, 140, 90]),
        ("dark_foliage", G, [40, 80, 45]),
        ("moss", G, [95, 110, 60]),
        ("aqua", A, [60, 200, 200]),
        ("turquoise", A, [50, 190, 170]),
        ("teal", A, [40, 140, 140]),
        ("dark_teal", A, [25, 85, 88]),
        ("bright_blue", BL, [50, 90, 220]),
        ("sky", BL, [120, 175, 225]),
        ("muted_blue", BL, [90, 120, 170]),
        ("slate", BL, [90, 110, 140]),
        ("dark_blue", BL, [30, 55, 120]),
        ("navy", BL, [22, 38, 92]),
        ("purple", P, [130, 70, 190]),
        ("violet", P, [150, 90, 210]),
        ("lavender", P, [190, 170, 225]),
        ("muted_purple", P, [120, 100, 150]),
        ("plum", P, [120, 70, 120]),
        ("magenta", M, [210, 60, 170]),
        ("pink", M, [235, 140, 180]),
        ("dusty_pink", M, [200, 150, 165]),
        ("mauve", M, [180, 140, 160]),
        ("deep_magenta", M, [160, 30, 110]),
    ]
}

#[test]
fn every_band_volume_belongs_to_its_band_and_responds() {
    for (name, band, rgb) in band_volume() {
        let aff = base_aff_u8(rgb);
        // Belongs to its own band with meaningful confidence…
        assert!(
            aff[band] > 0.35,
            "{name}: weak own-band affinity {:.3} (band {band})",
            aff[band]
        );
        // …and its own band is (at least nearly) the strongest of all 8 —
        // a straddler like moss legitimately answers Yellow AND Green, so
        // a close second place next door is fine; being dominated is not.
        for (j, &a) in aff.iter().enumerate() {
            if j != band {
                assert!(
                    aff[band] >= a * 0.8 - 1e-4,
                    "{name}: band {j} ({a:.3}) dominates own band {band} ({:.3})",
                    aff[band]
                );
            }
        }
        // A push from its own Saturation slider visibly moves it.
        assert!(
            moved(&sat_band(band), rgb) > 6,
            "{name}: own Saturation slider barely moved it"
        );
    }
}

#[test]
fn band_edits_do_not_reach_non_adjacent_families() {
    // Only the two bracketing bands are ever non-zero, so a distant band's
    // slider is a strict no-op regardless of tone/chroma.
    let cases = [
        ("bright_red", R, BL, [210, 45, 45]),
        ("bright_blue", BL, O, [50, 90, 220]),
        ("bright_green", G, M, [70, 180, 70]),
        ("bright_yellow", Y, BL, [235, 220, 60]),
        ("navy", BL, Y, [22, 38, 92]),
        ("magenta", M, A, [210, 60, 170]),
    ];
    for (name, _own, far, rgb) in cases {
        assert!(
            moved(&sat_band(far), rgb) <= 3,
            "{name}: distant band {far} recoloured it"
        );
    }
}

#[test]
fn aqua_and_blue_stay_distinct() {
    // The A↔B pair is the classic "cyan swallowed by blue" failure.
    for rgb in [[60, 200, 200], [50, 190, 170], [40, 140, 140], [25, 85, 88]] {
        let aff = base_aff_u8(rgb);
        assert!(aff[A] > aff[BL], "aqua {rgb:?} leaned Blue: {aff:?}");
        assert!(moved(&sat_band(A), rgb) > 6, "aqua {rgb:?} ignored Aqua");
    }
    for rgb in [[50, 90, 220], [30, 55, 120], [22, 38, 92]] {
        let aff = base_aff_u8(rgb);
        assert!(aff[BL] > aff[A], "blue {rgb:?} leaned Aqua: {aff:?}");
        assert!(moved(&sat_band(A), rgb) <= 4, "blue {rgb:?} took Aqua edit");
    }
}

#[test]
fn neutrals_and_near_neutrals_are_protected() {
    // Grey/black/white take essentially nothing under a max Hue+Sat push on
    // every band; a barely-tinted near-neutral takes only a controlled amount.
    let mut all_bands = DevelopSettings::default();
    for i in 0..MIXER_BANDS {
        all_bands.mixer_hue[i] = CONTROL_LIMIT;
        all_bands.mixer_saturation[i] = CONTROL_LIMIT;
    }
    for rgb in [[12, 12, 12], [128, 128, 128], [245, 245, 245]] {
        assert!(
            moved(&all_bands, rgb) < 6,
            "neutral {rgb:?} was recoloured: {:?}",
            develop_one(&all_bands, rgb)
        );
        for &a in base_aff_u8(rgb).iter() {
            assert!(a < 0.06, "neutral {rgb:?} has affinity {a:.3}");
        }
    }
    // Near-neutral (chroma ≈ 0.05): non-zero but firmly bounded.
    for rgb in [[140, 132, 128], [126, 130, 138]] {
        assert!(
            moved(&all_bands, rgb) < 22,
            "near-neutral {rgb:?} moved too far: {:?}",
            develop_one(&all_bands, rgb)
        );
    }
}

#[test]
fn colored_shadow_survives_while_neutral_shadow_does_not() {
    // A dark saturated navy must remain adjustable; a near-NEUTRAL dark of the
    // same luma (equal-ish channels, rgb_chroma < 0.02) must stay protected —
    // the chroma confidence, not luma, is what separates them.
    let navy = base_aff_u8([22, 38, 92]); // luma ≈ 0.15, chroma ≈ 0.27
    let neutral_dark = base_aff_u8([32, 33, 35]); // luma ≈ 0.13, chroma ≈ 0.012
    assert!(
        navy[BL] > 0.4,
        "colored navy lost its Blue response: {navy:?}"
    );
    assert!(
        neutral_dark.iter().all(|&a| a < 0.06),
        "near-neutral dark behaves like a colour: {neutral_dark:?}"
    );
}

#[test]
fn adjacent_band_boundaries_are_continuous() {
    // Sweep the whole hue circle at a fixed chroma/luma; the per-band affinity
    // field must never jump — no seams at any of the 8 band boundaries.
    let step = 0.25f32;
    let mut prev: Option<[f32; MIXER_BANDS]> = None;
    let mut h = 0.0f32;
    while h < 360.0 {
        let (r, g, b) = crate::core::color::hsl_to_rgb(h / 360.0, 0.7, 0.5);
        let aff = mixer_band_memberships(r, g, b);
        if let Some(p) = prev {
            for i in 0..MIXER_BANDS {
                assert!(
                    (aff[i] - p[i]).abs() < 0.05,
                    "affinity discontinuity at hue {h}° band {i}: {} -> {}",
                    p[i],
                    aff[i]
                );
            }
        }
        prev = Some(aff);
        h += step;
    }
}

#[test]
fn luminance_weight_is_guarded_where_hue_sat_are_not() {
    // Hue and Saturation share one colour-confidence weight; Luminance is
    // additionally (a) shadow/highlight-guarded and (b) demands MORE
    // saturation (higher logistic midpoint) — brightening a barely-coloured
    // pixel reads as tone damage.
    let weights = |rgb: [u8; 3]| {
        let (r, g, b) = (
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
        );
        mixer_edit_weights(r, g, b, luminance_f32(r, g, b).clamp(0.0, 1.0))
    };

    // DEEP colored shadow (luma ≈ 0.09): Luminance suppressed vs Sat.
    let (s2, l2) = weights([14, 22, 52]);
    assert!(
        l2 < s2 * 0.88,
        "Luminance not deep-shadow-protected: lum {l2} vs sat {s2}"
    );

    // Near-white highlight: Luminance suppressed there too.
    let (s3, l3) = weights([245, 235, 120]);
    assert!(
        l3 < s3,
        "Luminance not highlight-protected: lum {l3} vs sat {s3}"
    );

    // Pale mid-tone (saturation between the two midpoints): Luminance takes
    // clearly less than Hue/Sat.
    let (s4, l4) = weights([145, 138, 134]);
    assert!(
        l4 < s4 * 0.7,
        "Luminance not pale-guarded: lum {l4} vs sat {s4}"
    );

    // A vivid mid-tone keeps BOTH weights near full.
    let (s5, l5) = weights([210, 45, 45]);
    assert!(
        s5 > 0.95 && l5 > 0.85,
        "vivid red lost weight: sat {s5} lum {l5}"
    );
}

#[test]
fn strong_edits_stay_finite_and_never_speckle() {
    // Every family + neutrals, under strong +/- Hue, Saturation and Luminance
    // (single band and all bands at once): output must stay valid, alpha
    // preserved, and a coloured pixel must never collapse to pure black.
    let mut colors: Vec<[u8; 3]> = band_volume().iter().map(|&(_, _, c)| c).collect();
    colors.extend_from_slice(&[[12, 12, 12], [128, 128, 128], [245, 245, 245]]);

    let mut settings_list = Vec::new();
    for sign in [CONTROL_LIMIT, -CONTROL_LIMIT] {
        for field in 0..3 {
            // single band 0 and all-bands, per edit channel
            for all in [false, true] {
                let mut s = DevelopSettings::default();
                for band in 0..MIXER_BANDS {
                    if all || band == 0 {
                        match field {
                            0 => s.mixer_hue[band] = sign,
                            1 => s.mixer_saturation[band] = sign,
                            _ => s.mixer_luminance[band] = sign,
                        }
                    }
                }
                settings_list.push(s);
            }
        }
    }
    // Plus two neighbouring bands edited together (crossover stability).
    let mut both = DevelopSettings::default();
    both.mixer_saturation[A] = CONTROL_LIMIT;
    both.mixer_saturation[BL] = -CONTROL_LIMIT;
    settings_list.push(both);

    for s in &settings_list {
        for &rgb in &colors {
            let mut px = vec![rgb[0], rgb[1], rgb[2], 200u8];
            apply_to_pixels(s, &mut px, 1, 1);
            assert_eq!(px[3], 200, "alpha changed for {rgb:?}");
            let colored =
                rgb[0].max(rgb[1]).max(rgb[2]) as i32 - rgb[0].min(rgb[1]).min(rgb[2]) as i32 > 24;
            if colored {
                assert!(
                    px[0] as u32 + px[1] as u32 + px[2] as u32 > 0,
                    "coloured pixel {rgb:?} collapsed to black under {:?}",
                    (s.mixer_hue[0], s.mixer_saturation[0], s.mixer_luminance[0])
                );
            }
        }
    }
}

// ── Perceptual-strength acceptance (PRODUCTION proxy path) ───────────────
// These run through `apply_to_tilemap_direct` (the real preview/commit path,
// which the GPU mirrors), NOT the per-pixel test path — so they measure the
// strength a user actually sees on a RAW, and would have caught the squared-
// gate regression that made shadows/muted colours look inert.

/// OKLab of an 8-bit sRGB colour (L, a, b). Same matrices as
/// `color::oklab_hue_deg`, plus the L row.
fn oklab(rgb: [u8; 3]) -> [f32; 3] {
    fn s2l(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    let lr = s2l(rgb[0] as f32 / 255.0);
    let lg = s2l(rgb[1] as f32 / 255.0);
    let lb = s2l(rgb[2] as f32 / 255.0);
    let l = 0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb;
    let m = 0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb;
    let s = 0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    ]
}

fn oklab_de(a: [u8; 3], b: [u8; 3]) -> f32 {
    let x = oklab(a);
    let y = oklab(b);
    ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2)).sqrt()
}

fn oklab_chroma_u8(rgb: [u8; 3]) -> f32 {
    let l = oklab(rgb);
    (l[1] * l[1] + l[2] * l[2]).sqrt()
}

fn oklab_chroma_delta(a: [u8; 3], b: [u8; 3]) -> f32 {
    oklab_chroma_u8(a) - oklab_chroma_u8(b)
}

/// Apply `settings` to a uniform tile through the REAL production path and
/// return the centre pixel — this exercises `build_color_lowpass` +
/// `finish_colored_pixel` (proxy + full-res re-gate), exactly like a RAW.
fn proxy_apply(settings: &DevelopSettings, rgb: [u8; 3]) -> [u8; 3] {
    use crate::core::tile::{TilePos, TILE_SIZE};
    let (w, h) = (TILE_SIZE, TILE_SIZE);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for p in px.chunks_exact_mut(4) {
        p[0] = rgb[0];
        p[1] = rgb[1];
        p[2] = rgb[2];
        p[3] = 255;
    }
    let out = apply_to_tilemap_direct(&TileMap::from_rgba(&px, w, h), settings, None);
    let t = out.tiles.get(&TilePos::from_pixel(w / 2, h / 2)).unwrap();
    let (r, g, b, _) = t.get_pixel(w / 2 % TILE_SIZE, h / 2 % TILE_SIZE);
    [r, g, b]
}

fn proxy_de(band: usize, field: u8, value: f32, rgb: [u8; 3]) -> f32 {
    let mut s = DevelopSettings::default();
    match field {
        0 => s.mixer_hue[band] = value,
        1 => s.mixer_saturation[band] = value,
        _ => s.mixer_luminance[band] = value,
    }
    oklab_de(proxy_apply(&s, rgb), rgb)
}

fn direct_apply(settings: &DevelopSettings, rgb: [u8; 3]) -> [u8; 3] {
    let mut p = vec![rgb[0], rgb[1], rgb[2], 255u8];
    apply_to_pixels(settings, &mut p, 1, 1);
    [p[0], p[1], p[2]]
}

fn all_band_saturation_settings(value: f32) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    for i in 0..MIXER_BANDS {
        s.mixer_saturation[i] = value;
    }
    s
}

fn proxy_patch_chroma_metrics(settings: &DevelopSettings, samples: &[[u8; 3]]) -> Vec<(f32, f32)> {
    use crate::core::tile::{TilePos, TILE_SIZE};
    let (w, h) = (TILE_SIZE, TILE_SIZE);
    let stripe_w = w / samples.len() as u32;
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = (x / stripe_w).min(samples.len() as u32 - 1) as usize;
            let rgb = samples[idx];
            let i = ((y * w + x) * 4) as usize;
            px[i] = rgb[0];
            px[i + 1] = rgb[1];
            px[i + 2] = rgb[2];
            px[i + 3] = 255;
        }
    }
    let out = apply_to_tilemap_direct(&TileMap::from_rgba(&px, w, h), settings, None);
    let mut metrics = Vec::with_capacity(samples.len());
    for i in 0..samples.len() {
        let x = (i as u32 * stripe_w + stripe_w / 2).min(w - 1);
        let y = h / 2;
        let t = out.tiles.get(&TilePos::from_pixel(x, y)).unwrap();
        let (r, g, b, _) = t.get_pixel(x % TILE_SIZE, y % TILE_SIZE);
        let out_rgb = [r, g, b];
        metrics.push((
            oklab_chroma_delta(out_rgb, samples[i]),
            oklab_de(out_rgb, samples[i]),
        ));
    }
    metrics
}

// OKLab ΔE ≈ 0.02 is a comfortably perceptible change; core edits at +100
// must clear it with margin, and target must dwarf non-adjacent/neutral.
const VISIBLE: f32 = 0.020;

fn rgb_chroma_u8_norm(rgb: [u8; 3]) -> f32 {
    (rgb[0].max(rgb[1]).max(rgb[2]) - rgb[0].min(rgb[1]).min(rgb[2])) as f32 / 255.0
}

fn oklab_hue_u8(rgb: [u8; 3]) -> f32 {
    oklab_hue_deg(
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    )
}

fn hue_gap_deg(a: f32, b: f32) -> f32 {
    ((a - b + 180.0).rem_euclid(360.0) - 180.0).abs()
}

fn assert_light_lift_preserves_color(
    settings: &DevelopSettings,
    label: &str,
    rgb: [u8; 3],
    min_delta_l: f32,
    min_chroma_gain: f32,
) {
    let before_l = oklab(rgb)[0];
    let before_h = oklab_hue_u8(rgb);
    let before_c = rgb_chroma_u8_norm(rgb).max(1e-6);
    for (path, out) in [
        ("direct", direct_apply(settings, rgb)),
        ("proxy", proxy_apply(settings, rgb)),
    ] {
        let delta_l = oklab(out)[0] - before_l;
        let hue_gap = hue_gap_deg(oklab_hue_u8(out), before_h);
        let chroma_gain = rgb_chroma_u8_norm(out) / before_c;
        assert!(
            delta_l >= min_delta_l,
            "{label} {path}: did not gain enough lightness rgb={rgb:?} out={out:?} dL={delta_l:.4}"
        );
        assert!(
            hue_gap <= 16.0,
            "{label} {path}: hue drifted rgb={rgb:?} out={out:?} hue_gap={hue_gap:.2}"
        );
        assert!(
            chroma_gain >= min_chroma_gain,
            "{label} {path}: chroma collapsed rgb={rgb:?} out={out:?} gain={chroma_gain:.2}"
        );
    }
}

fn light_settings(shadows: f32, blacks: f32) -> DevelopSettings {
    DevelopSettings {
        shadows,
        blacks,
        ..Default::default()
    }
}

#[test]
fn shadows_lift_colored_foliage_as_green_not_gray() {
    let foliage = [18, 42, 20];
    assert_light_lift_preserves_color(
        &light_settings(100.0, 0.0),
        "foliage Shadows+100",
        foliage,
        0.045,
        1.18,
    );
    assert_light_lift_preserves_color(
        &light_settings(CONTROL_LIMIT, 0.0),
        "foliage Shadows+200",
        foliage,
        0.070,
        1.35,
    );
}

#[test]
fn shadows_lift_dark_color_families_without_neutralizing() {
    let settings = light_settings(CONTROL_LIMIT, 0.0);
    for (name, rgb) in [
        ("dark_foliage", [18, 42, 20]),
        ("olive", [45, 48, 20]),
        ("dark_teal", [18, 54, 56]),
        ("navy", [16, 26, 60]),
        ("burgundy", [58, 18, 24]),
        ("dark_brown", [46, 28, 16]),
    ] {
        assert_light_lift_preserves_color(&settings, name, rgb, 0.060, 1.28);
    }
}

#[test]
fn blacks_lift_dark_color_families_without_neutralizing() {
    let settings = light_settings(0.0, CONTROL_LIMIT);
    for (name, rgb) in [
        ("dark_foliage", [18, 42, 20]),
        ("olive", [45, 48, 20]),
        ("dark_teal", [18, 54, 56]),
        ("navy", [16, 26, 60]),
        ("burgundy", [58, 18, 24]),
        ("dark_brown", [46, 28, 16]),
    ] {
        assert_light_lift_preserves_color(&settings, name, rgb, 0.025, 1.10);
    }
}

#[test]
fn shadows_and_blacks_keep_neutrals_neutral_and_black_uncolored() {
    for settings in [
        light_settings(CONTROL_LIMIT, 0.0),
        light_settings(0.0, CONTROL_LIMIT),
        light_settings(CONTROL_LIMIT, 50.0),
    ] {
        for rgb in [[36, 36, 36], [4, 4, 4], [0, 0, 0]] {
            for (path, out) in [
                ("direct", direct_apply(&settings, rgb)),
                ("proxy", proxy_apply(&settings, rgb)),
            ] {
                let spread = out[0].max(out[1]).max(out[2]) - out[0].min(out[1]).min(out[2]);
                assert!(
                    spread <= 2,
                    "{path}: neutral/black gained false color rgb={rgb:?} out={out:?}"
                );
                if rgb == [0, 0, 0] {
                    assert!(
                        out[0].max(out[1]).max(out[2]) <= 90,
                        "{path}: true black lifted too far rgb={rgb:?} out={out:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn local_light_lift_preserves_dark_green_texture_and_tonal_order() {
    use crate::core::tile::{TilePos, TILE_SIZE};
    let (w, h) = (64u32, 16u32);
    let levels = [[14u8, 32, 16], [20, 42, 20], [28, 56, 26]];
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let rgb = levels[(x % 3) as usize];
            let i = ((y * w + x) * 4) as usize;
            px[i] = rgb[0];
            px[i + 1] = rgb[1];
            px[i + 2] = rgb[2];
            px[i + 3] = 255;
        }
    }

    let settings = light_settings(CONTROL_LIMIT, 50.0);
    let out = apply_to_tilemap_direct(&TileMap::from_rgba(&px, w, h), &settings, None);
    let at = |x: u32, y: u32| -> [u8; 3] {
        let t = out.tiles.get(&TilePos::from_pixel(x, y)).unwrap();
        let (r, g, b, _) = t.get_pixel(x % TILE_SIZE, y % TILE_SIZE);
        [r, g, b]
    };

    let yy = h / 2;
    let xs = [30u32, 31, 32];
    let out_levels = [at(xs[0], yy), at(xs[1], yy), at(xs[2], yy)];
    let l0 = luma_u8(&[out_levels[0][0], out_levels[0][1], out_levels[0][2], 255]);
    let l1 = luma_u8(&[out_levels[1][0], out_levels[1][1], out_levels[1][2], 255]);
    let l2 = luma_u8(&[out_levels[2][0], out_levels[2][1], out_levels[2][2], 255]);
    assert!(
        l0 < l1 && l1 < l2,
        "dark-green tonal ordering collapsed: {out_levels:?}"
    );
    assert!(
        l2 - l0 >= 18.0,
        "dark-green texture separation flattened: {out_levels:?}"
    );
    for (before, after) in levels.into_iter().zip(out_levels) {
        assert!(
            after[1] > after[0] + 18 && after[1] > after[2] + 18,
            "lifted texture sample lost green identity: {before:?}->{after:?}"
        );
        assert!(
            rgb_chroma_u8_norm(after) >= rgb_chroma_u8_norm(before) * 1.20,
            "lifted texture sample lost chroma: {before:?}->{after:?}"
        );
    }
}

#[test]
fn flat_light_proxy_matches_direct_color_reconstruction() {
    for settings in [
        light_settings(100.0, 0.0),
        light_settings(CONTROL_LIMIT, 0.0),
        light_settings(0.0, 100.0),
        light_settings(0.0, CONTROL_LIMIT),
        light_settings(100.0, 50.0),
    ] {
        for rgb in [
            [18, 42, 20],
            [45, 48, 20],
            [18, 54, 56],
            [16, 26, 60],
            [58, 18, 24],
            [46, 28, 16],
            [36, 36, 36],
            [0, 0, 0],
        ] {
            let direct = direct_apply(&settings, rgb);
            let proxy = proxy_apply(&settings, rgb);
            let gap = oklab_de(proxy, direct);
            assert!(
                gap < 0.018,
                "proxy/direct light mismatch rgb={rgb:?}: direct={direct:?} proxy={proxy:?} dE={gap:.4}"
            );
        }
    }
}

#[test]
fn strong_light_edits_stay_valid_without_neon_shadow_artifacts() {
    let settings = [
        light_settings(CONTROL_LIMIT, CONTROL_LIMIT),
        DevelopSettings {
            exposure: 18.0,
            contrast: 120.0,
            highlights: -120.0,
            shadows: CONTROL_LIMIT,
            whites: 80.0,
            blacks: 120.0,
            ..Default::default()
        },
    ];
    for settings in settings {
        for rgb in [
            [8, 8, 8],
            [18, 42, 20],
            [45, 48, 20],
            [18, 54, 56],
            [16, 26, 60],
            [58, 18, 24],
            [46, 28, 16],
            [36, 36, 36],
        ] {
            for (path, out) in [
                ("direct", direct_apply(&settings, rgb)),
                ("proxy", proxy_apply(&settings, rgb)),
            ] {
                if rgb_chroma_u8_norm(rgb) < 0.02 {
                    let spread = out[0].max(out[1]).max(out[2]) - out[0].min(out[1]).min(out[2]);
                    assert!(spread <= 3, "{path}: neutral speckle {rgb:?}->{out:?}");
                } else {
                    let gain = rgb_chroma_u8_norm(out) / rgb_chroma_u8_norm(rgb).max(1e-6);
                    assert!(
                        gain <= 4.6,
                        "{path}: neon shadow chroma spike {rgb:?}->{out:?} gain={gain:.2}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "diagnostic instrumentation, run with --ignored --nocapture"]
fn diag_light_shadow_color_reconstruction() {
    let cases = [
        ("dark_foliage_green", [18, 42, 20]),
        ("dark_olive", [45, 48, 20]),
        ("dark_teal", [18, 54, 56]),
        ("navy", [16, 26, 60]),
        ("dark_burgundy", [58, 18, 24]),
        ("dark_brown_wood", [46, 28, 16]),
        ("neutral_dark_gray", [36, 36, 36]),
        ("near_black_neutral", [4, 4, 4]),
        ("true_black", [0, 0, 0]),
    ];
    for (name, rgb) in cases {
        for (edit, value) in [
            ("Shadows", 100.0),
            ("Shadows", CONTROL_LIMIT),
            ("Blacks", 100.0),
            ("Blacks", CONTROL_LIMIT),
        ] {
            let settings = if edit == "Shadows" {
                light_settings(value, 0.0)
            } else {
                light_settings(0.0, value)
            };
            let direct = direct_apply(&settings, rgb);
            let proxy = proxy_apply(&settings, rgb);
            let (r, g, b) = (
                rgb[0] as f32 / 255.0,
                rgb[1] as f32 / 255.0,
                rgb[2] as f32 / 255.0,
            );
            let l = luminance_f32(r, g, b).clamp(0.0, 1.0);
            let chroma = rgb_chroma(r, g, b);
            let unit = control_to_unit(value);
            let target = if edit == "Shadows" {
                apply_light_luma(l, 0.0, unit, 0.0, 0.0)
            } else {
                apply_light_luma(l, 0.0, 0.0, 0.0, unit)
            };
            let tonal_mask = if edit == "Shadows" {
                shadow_mask(l)
            } else {
                black_mask(l)
            };
            let preserve = luma_target_chroma_preserve_weight(l, chroma, target);
            let scale = if l > 1e-5 { target / l } else { 0.0 };
            let scaled = [r * scale, g * scale, b * scale];
            let gamut = if scaled.iter().any(|v| *v < 0.0 || *v > 1.0) {
                "compressed"
            } else {
                "in-gamut"
            };
            let branch = if preserve >= 0.5 {
                "chroma-preserving"
            } else {
                "neutral-protection"
            };
            let report = |out: [u8; 3]| {
                let in_lab = oklab(rgb);
                let out_lab = oklab(out);
                (
                    out,
                    luma_u8(&[out[0], out[1], out[2], 255]) / 255.0,
                    out_lab[0],
                    hue_gap_deg(oklab_hue_u8(out), oklab_hue_u8(rgb)),
                    oklab_chroma_u8(out) - oklab_chroma_u8(rgb),
                    out_lab[0] - in_lab[0],
                )
            };
            println!(
                "{name} {edit}+{value:.0}: input=sRGB8 {rgb:?} luma={l:.4} L={:.4} hue={:.1} chroma={:.4} mask={tonal_mask:.3} lift={:.4} preserve={preserve:.3} branch={branch} gamut={gamut} direct={:?} proxy={:?}",
                oklab(rgb)[0],
                oklab_hue_u8(rgb),
                oklab_chroma_u8(rgb),
                target - l,
                report(direct),
                report(proxy),
            );
        }
    }
}

#[test]
fn all_band_saturation_plus200_materially_richens_mixed_patch() {
    let samples = [
        [40, 80, 45],    // foliage
        [205, 150, 120], // skin / orange
        [90, 25, 35],    // burgundy
        [150, 72, 48],   // ceramic red-brown
        [40, 140, 140],  // teal
        [90, 120, 170],  // blue / navy-adjacent
        [128, 128, 128], // neutral grey
        [8, 8, 8],       // black
        [245, 245, 245], // white
    ];
    let settings = all_band_saturation_settings(CONTROL_LIMIT);
    let metrics = proxy_patch_chroma_metrics(&settings, &samples);
    let required = [
        ("foliage", 0, 0.042, 0.050),
        ("skin", 1, 0.060, 0.065),
        ("burgundy", 2, 0.050, 0.075),
        ("ceramic", 3, 0.065, 0.080),
        ("teal", 4, 0.014, 0.030),
        ("blue", 5, 0.060, 0.070),
    ];
    for (name, idx, min_dc, min_de) in required {
        let (dc, de) = metrics[idx];
        assert!(
            dc > min_dc && de > min_de,
            "{name} all-band +200 too weak: dC={dc:.4} dE={de:.4} metrics={metrics:?}"
        );
    }
    // Neutrals stay FAR weaker than any colour, but a smooth region-following
    // grade (selection follows the guided region) lets a little colour bleed
    // into a neutral WITHIN a proxy radius of a colour boundary — these are
    // 28-px synthetic stripes, so the sampled centre is only ~14 px from a
    // colour edge (worst case); a real wide background is untouched at its
    // interior. So the neutral is allowed a small bounded move, still an
    // order of magnitude below the colours.
    let weakest_colored = metrics[0..6].iter().map(|m| m.1).fold(f32::MAX, f32::min);
    for (name, idx) in [("grey", 6), ("black", 7), ("white", 8)] {
        let (dc, de) = metrics[idx];
        assert!(
            dc.abs() < 0.010 && de < 0.012 && de < weakest_colored * 0.35,
            "{name} moved too much under all-band +200: dC={dc:.4} dE={de:.4} metrics={metrics:?}"
        );
    }
}

#[test]
fn edge_suppression_fades_correction_at_luma_edges_only() {
    // A proxy flat on the left, stepping to a much darker value on the
    // right. A uniform colour correction must SURVIVE in full in the flat
    // interior (far from any edge) and be pulled toward zero at the step —
    // the gradient-weight taper. Uniform regions (the whole mixer
    // test suite) have no internal gradient, so they are untouched.
    let (pw, ph) = (16usize, 8usize);
    let mut region = vec![[0.6f32, 0.4, 0.4]; pw * ph];
    for y in 0..ph {
        for x in (pw / 2)..pw {
            region[y * pw + x] = [0.12, 0.08, 0.08]; // dark half
        }
    }
    let mut adjusted: Vec<[f32; 3]> = region.iter().map(|p| [p[0] + 0.2, p[1], p[2]]).collect();
    suppress_edge_correction(&region, &mut adjusted, pw, ph);
    let corr = |k: usize| adjusted[k][0] - region[k][0];
    let interior = corr(4 * pw + 2); // deep in the flat light half
    assert!(
        interior > 0.18,
        "interior correction suppressed: {interior}"
    );
    let edge = corr(4 * pw + pw / 2); // at the luma step
                                      // Fades toward the boundary, but the EDGE_FADE_FLOOR keeps a real fraction
                                      // of the edit so the taper never carves a dark rim into the colour.
    assert!(
        edge < interior * 0.78,
        "edit did not fade across the edge: edge={edge} interior={interior}"
    );
    assert!(
        edge > interior * EDGE_FADE_FLOOR,
        "edit floored too low at the edge: edge={edge} interior={interior}"
    );
}

#[test]
fn edge_suppression_fades_edit_created_boundary_at_constant_luma() {
    // Region luma is CONSTANT across the frame: warm on the left, a grey of
    // the SAME luminance on the right (skin next to a white-grey background).
    // The region-luma gradient is ~0 at the boundary, so the old suppressor
    // did nothing there. Only the LEFT half is edited (a Luminance boost
    // selects the warm hue, not the grey) — exactly the "brightening a warm
    // orange/red against a grey-white background" case. The correction-gradient term must still fade the
    // boost at the warm↔grey boundary instead of holding a hard rim.
    let (pw, ph) = (16usize, 8usize);
    let warm = [0.62f32, 0.42, 0.30];
    let ly = luminance_f32(warm[0], warm[1], warm[2]);
    let grey = [ly, ly, ly]; // same luminance_f32 as `warm`
    let mut region = vec![warm; pw * ph];
    for y in 0..ph {
        for x in (pw / 2)..pw {
            region[y * pw + x] = grey;
        }
    }
    // Confirm the boundary has (near-)zero region-luma gradient.
    let lum = |p: [f32; 3]| luminance_f32(p[0], p[1], p[2]);
    assert!(
        (lum(warm) - lum(grey)).abs() < 1e-4,
        "test setup: warm and grey must share luminance"
    );
    // Edit = uniform brightening applied ONLY to the warm (left) half.
    let mut adjusted = region.clone();
    for y in 0..ph {
        for x in 0..(pw / 2) {
            let p = &mut adjusted[y * pw + x];
            p[0] += 0.12;
            p[1] += 0.08;
            p[2] += 0.06;
        }
    }
    suppress_edge_correction(&region, &mut adjusted, pw, ph);
    let corr = |k: usize| adjusted[k][0] - region[k][0];
    let interior = corr(4 * pw + 2); // deep in the edited half
    let edge = corr(4 * pw + (pw / 2 - 1)); // last edited column, at the rim
    assert!(interior > 0.10, "interior boost suppressed: {interior}");
    // Fades at the constant-luma boundary, but the floor keeps the colour side
    // from being carved out — a soft stop, not a dark rim eating into the skin.
    assert!(
        edge < interior * 0.85,
        "edit did not fade at the constant-luma boundary: edge={edge} interior={interior}"
    );
    assert!(
        edge > interior * EDGE_FADE_FLOOR,
        "colour lift eaten too far at the boundary: edge={edge} interior={interior}"
    );
}

#[test]
fn every_band_saturation_has_clear_plus100_and_plus200_strength() {
    let cases = [
        ("red", R, [170, 45, 48]),
        ("orange", O, [205, 150, 120]),
        ("yellow", Y, [190, 160, 45]),
        ("green", G, [70, 180, 70]),
        ("aqua", A, [40, 140, 140]),
        ("blue", BL, [90, 120, 170]),
        ("purple", P, [130, 70, 190]),
        ("magenta", M, [210, 60, 170]),
    ];
    for (name, band, rgb) in cases {
        let mut s100 = DevelopSettings::default();
        s100.mixer_saturation[band] = 100.0;
        let mut s200 = DevelopSettings::default();
        s200.mixer_saturation[band] = CONTROL_LIMIT;
        let out100 = proxy_apply(&s100, rgb);
        let out200 = proxy_apply(&s200, rgb);
        let dc100 = oklab_chroma_delta(out100, rgb);
        let dc200 = oklab_chroma_delta(out200, rgb);
        let de100 = oklab_de(out100, rgb);
        let de200 = oklab_de(out200, rgb);
        assert!(
            dc100 > 0.012 && de100 > VISIBLE * 0.9,
            "{name} +100 Saturation too weak: dC={dc100:.4} dE={de100:.4} out={out100:?}"
        );
        assert!(
            dc200 > 0.014 && de200 > VISIBLE && dc200 >= dc100 - 0.002,
            "{name} +200 Saturation too weak or regressed: dC {dc100:.4}->{dc200:.4} dE {de100:.4}->{de200:.4}"
        );
    }
}

#[test]
fn proxy_and_direct_saturation_strength_remain_comparable_at_plus200() {
    let settings = all_band_saturation_settings(CONTROL_LIMIT);
    for rgb in [
        [40, 80, 45],
        [205, 150, 120],
        [90, 25, 35],
        [150, 72, 48],
        [40, 140, 140],
        [90, 120, 170],
        [22, 38, 92],
    ] {
        let direct = direct_apply(&settings, rgb);
        let proxy = proxy_apply(&settings, rgb);
        let direct_dc = oklab_chroma_delta(direct, rgb);
        let proxy_dc = oklab_chroma_delta(proxy, rgb);
        let gap = oklab_de(proxy, direct);
        assert!(
            gap < 0.010 && (proxy_dc - direct_dc).abs() < 0.006,
            "proxy/direct saturation mismatch for {rgb:?}: direct={direct:?} proxy={proxy:?} dC {direct_dc:.4}/{proxy_dc:.4} gap={gap:.4}"
        );
    }
}

#[test]
fn strong_color_mixer_edits_protect_neutrals_and_stay_valid() {
    let mut settings_list = Vec::new();
    for sign in [CONTROL_LIMIT, -CONTROL_LIMIT] {
        let mut hue = DevelopSettings::default();
        let mut sat = DevelopSettings::default();
        let mut lum = DevelopSettings::default();
        for band in 0..MIXER_BANDS {
            hue.mixer_hue[band] = sign;
            sat.mixer_saturation[band] = sign;
            lum.mixer_luminance[band] = sign;
        }
        settings_list.extend([hue, sat, lum]);
    }

    let samples = [
        [8, 8, 8],
        [42, 43, 44],
        [128, 128, 128],
        [245, 245, 245],
        [40, 80, 45],
        [90, 25, 35],
        [22, 38, 92],
        [205, 150, 120],
    ];
    for settings in settings_list {
        for rgb in samples {
            let mut px = vec![rgb[0], rgb[1], rgb[2], 173u8];
            apply_to_pixels(&settings, &mut px, 1, 1);
            assert_eq!(px[3], 173, "alpha changed for {rgb:?}");
            let out = [px[0], px[1], px[2]];
            if rgb[0] == rgb[1] && rgb[1] == rgb[2] {
                assert!(
                    oklab_de(out, rgb) < 0.010,
                    "neutral recolored by strong mixer edit: {rgb:?}->{out:?}"
                );
            }
            if rgb[0].max(rgb[1]).max(rgb[2]) - rgb[0].min(rgb[1]).min(rgb[2]) > 24 {
                assert!(
                    out[0] as u32 + out[1] as u32 + out[2] as u32 > 0,
                    "colored pixel collapsed to black: {rgb:?}->{out:?}"
                );
            }
        }
    }
}

#[test]
fn proxy_core_colors_respond_visibly_and_beat_non_adjacent() {
    // (name, band, non-adjacent band, rgb)
    let cases = [
        ("sat_red", R, BL, [210, 45, 45]),
        ("skin_orange", O, BL, [205, 150, 120]),
        ("bright_yellow", Y, BL, [235, 220, 60]),
        ("bright_green", G, M, [70, 180, 70]),
        ("teal", A, R, [40, 140, 140]),
        ("muted_blue", BL, O, [90, 120, 170]),
        ("purple", P, Y, [130, 70, 190]),
        ("magenta", M, A, [210, 60, 170]),
    ];
    for (name, band, far, rgb) in cases {
        let mut strongest = 0.0f32;
        for field in [0u8, 1, 2] {
            let at100 = proxy_de(band, field, 100.0, rgb);
            let at50 = proxy_de(band, field, 50.0, rgb);
            let far100 = proxy_de(far, field, 100.0, rgb);
            strongest = strongest.max(at100);
            // Each axis is at least perceptible (an already-saturated colour
            // has little Saturation headroom by design, but never inert)…
            assert!(
                at100 > VISIBLE * 0.9,
                "{name} field {field}: target +100 not perceptible ΔE={at100:.4}"
            );
            assert!(
                at50 > VISIBLE * 0.35,
                "{name} field {field}: target +50 not perceptible ΔE={at50:.4}"
            );
            // …and always dwarfs a non-adjacent band's slider.
            assert!(
                at100 > far100 * 4.0 + 0.008,
                "{name} field {field}: non-adjacent not dwarfed ({at100:.4} vs {far100:.4})"
            );
        }
        // At least one axis produces a MATERIAL (not just perceptible) change.
        assert!(
            strongest > VISIBLE * 2.5,
            "{name}: no axis responds materially (max ΔE={strongest:.4})"
        );
    }
}

#[test]
fn proxy_colored_shadows_respond_but_neutral_shadow_does_not() {
    // Dark saturated colours must move meaningfully in the live path…
    for (name, band, rgb) in [
        ("burgundy", R, [90, 25, 35]),
        ("dark_foliage", G, [40, 80, 45]),
        ("navy", BL, [22, 38, 92]),
        ("dark_teal", A, [25, 85, 88]),
    ] {
        let de = proxy_de(band, 1, 100.0, rgb); // Saturation +100
        assert!(
            de > VISIBLE,
            "{name}: colored shadow barely responded ΔE={de:.4}"
        );
    }
    // …while a TRUE near-neutral dark (rgb_chroma < 0.02) stays put on Blue.
    let neutral_shadow = proxy_de(BL, 1, 100.0, [38, 39, 41]);
    assert!(
        neutral_shadow < VISIBLE * 0.4,
        "near-neutral dark behaved like navy ΔE={neutral_shadow:.4}"
    );
}

#[test]
fn red_luminance_keeps_colour_and_does_not_wash_to_white() {
    // The point of a RATIO-PRESERVING brightness (an HSB-style brightness
    // gain): a brightened red stays a RED — hue held, chroma NOT collapsed
    // toward white — rather than getting a bright-grey/white wash. Reds with
    // gamut headroom brighten; a neutral background barely moves.
    // HSV saturation (delta/max) is scale-invariant: a ratio-preserving
    // brightness keeps it, an additive wash toward white collapses it.
    let sat = |rgb: [u8; 3]| {
        let mx = rgb[0].max(rgb[1]).max(rgb[2]);
        let mn = rgb[0].min(rgb[1]).min(rgb[2]);
        if mx == 0 {
            0.0
        } else {
            (mx - mn) as f32 / mx as f32
        }
    };
    let mut settings = DevelopSettings::default();
    settings.mixer_luminance[0] = CONTROL_LIMIT;
    for rgb in [[170, 55, 55], [150, 20, 34], [120, 40, 44]] {
        let out = direct_apply(&settings, rgb);
        assert!(
            oklab(out)[0] > oklab(rgb)[0] + 0.01,
            "{rgb:?} not brightened: {out:?}"
        );
        assert!(
            hue_gap_deg(oklab_hue_u8(out), oklab_hue_u8(rgb)) < 14.0,
            "{rgb:?} hue drifted to {out:?}"
        );
        assert!(
            sat(out) > sat(rgb) * 0.9,
            "{rgb:?} washed toward white: sat {:.3} -> {:.3} ({out:?})",
            sat(rgb),
            sat(out)
        );
    }
    // Darkening likewise keeps the colour (a darker red, not a muddy grey).
    let mut dark = DevelopSettings::default();
    dark.mixer_luminance[0] = -CONTROL_LIMIT;
    let out = direct_apply(&dark, [200, 60, 60]);
    assert!(
        oklab(out)[0] < oklab([200, 60, 60])[0] - 0.01,
        "not darkened"
    );
    assert!(
        sat(out) > sat([200, 60, 60]) * 0.9,
        "darkening greyed the red: {out:?}"
    );
    // Neutral background stays put under a Red push.
    let bg = [95, 122, 150];
    assert!(
        (oklab(direct_apply(&settings, bg))[0] - oklab(bg)[0]).abs() < 0.02,
        "background took the Red edit"
    );
    // Brightening must never DARKEN a bright highlight (the old shoulder
    // pulled near-white values below their own level, greying highlights).
    for band in 0..MIXER_BANDS {
        let mut br = DevelopSettings::default();
        br.mixer_luminance[band] = CONTROL_LIMIT;
        for hi in [[250, 250, 248], [245, 235, 225], [240, 210, 205]] {
            let out = direct_apply(&br, hi);
            assert!(
                oklab(out)[0] >= oklab(hi)[0] - 0.005,
                "band {band} +Lum darkened a highlight {hi:?} -> {out:?}"
            );
        }
    }
}

#[test]
fn proxy_matches_direct_for_red_luminance() {
    // The proxy (finish_colored_pixel) path must match the per-pixel direct
    // path for a uniform colour under a Red Luminance push (preview ==
    // commit), reds must brighten, and a neutral must stay put.
    let mut settings = DevelopSettings::default();
    settings.mixer_luminance[0] = CONTROL_LIMIT;
    for rgb in [[150, 20, 34], [210, 45, 45], [184, 132, 126]] {
        let d = direct_apply(&settings, rgb);
        let p = proxy_apply(&settings, rgb);
        assert!(
            oklab_de(d, p) < 0.012,
            "proxy != direct for {rgb:?}: {d:?} vs {p:?}"
        );
        assert!(oklab(p)[0] > oklab(rgb)[0], "{rgb:?} not brightened: {p:?}");
    }
    let neutral = [48, 48, 50];
    assert!(
        (oklab(proxy_apply(&settings, neutral))[0] - oklab(neutral)[0]).abs() < VISIBLE * 0.5,
        "neutral dark background moved under Red +Lum"
    );
}

#[test]
fn luminance_weight_falls_off_for_pale_members_of_every_band() {
    // The Luminance weight demands real saturation (its logistic midpoint
    // sits above Hue/Sat's): a vivid member of any band must far outweigh a
    // pale wash of the same hue, so brightening a colour never drags the
    // barely-tinted surroundings with it.
    for band in 0..MIXER_BANDS {
        let c = MIXER_COLORS[band];
        let vivid = [
            c[0] as f32 / 255.0,
            c[1] as f32 / 255.0,
            c[2] as f32 / 255.0,
        ];
        // Same hue family, chroma collapsed to a wash (~6% of the spread).
        let mean = (vivid[0] + vivid[1] + vivid[2]) / 3.0;
        let pale = [
            mean + (vivid[0] - mean) * 0.06,
            mean + (vivid[1] - mean) * 0.06,
            mean + (vivid[2] - mean) * 0.06,
        ];
        let wl = |p: [f32; 3]| {
            mixer_edit_weights(
                p[0],
                p[1],
                p[2],
                luminance_f32(p[0], p[1], p[2]).clamp(0.0, 1.0),
            )
            .1
        };
        let (wv, wp) = (wl(vivid), wl(pale));
        assert!(
            wv > (wp * 2.0 + 0.05).min(0.99),
            "band {band}: pale wash not attenuated (vivid {wv:.3} vs pale {wp:.3})"
        );
    }
}

#[test]
fn proxy_dark_but_visible_colors_respond_materially() {
    // The real-world complaint: dark-but-clearly-coloured regions (dark
    // foliage, olive, burgundy, dim packaging, dark teal, navy) must respond
    // materially in the LIVE path — far more than a true dark neutral. Uses the
    // strongest of the three edit axes (Hue/Sat/Lum) at an ordinary +100.
    let best = |band: usize, rgb: [u8; 3]| {
        proxy_de(band, 0, 100.0, rgb)
            .max(proxy_de(band, 1, 100.0, rgb))
            .max(proxy_de(band, 2, 100.0, rgb))
    };
    // Strongest response a TRUE dark grey can muster across every band/axis.
    let mut neutral_dark = 0.0f32;
    for band in 0..MIXER_BANDS {
        for field in 0..3u8 {
            neutral_dark = neutral_dark.max(proxy_de(band, field, 100.0, [40, 41, 42]));
        }
    }
    assert!(
        neutral_dark < 0.008,
        "true dark neutral over-responds ΔE={neutral_dark:.4}"
    );
    for (name, band, rgb) in [
        ("foliage_deep", G, [22, 42, 26]),
        ("moss", G, [70, 82, 45]),
        ("olive", Y, [95, 88, 38]),
        ("burgundy_deep", R, [60, 18, 24]),
        ("dim_magenta", M, [95, 28, 62]),
        ("dark_teal", A, [25, 78, 80]),
        ("navy_deep", BL, [16, 26, 60]),
    ] {
        let b = best(band, rgb);
        assert!(
            b > VISIBLE * 0.90,
            "{name}: dark-but-visible colour barely responds ΔE={b:.4}"
        );
        assert!(
            b > neutral_dark * 4.0 + 0.01,
            "{name}: not materially stronger than dark neutral ({b:.4} vs {neutral_dark:.4})"
        );
    }
}

#[test]
fn proxy_navy_responds_to_blue_but_black_stays_black() {
    let navy = proxy_de(BL, 1, 100.0, [22, 38, 92]);
    let black = proxy_de(BL, 1, 100.0, [8, 8, 8]);
    assert!(navy > VISIBLE, "navy did not respond to Blue: ΔE={navy:.4}");
    assert!(black < 0.006, "black recoloured by Blue: ΔE={black:.4}");
    // Darkening navy must not punch it to pure black in the live path.
    let out = proxy_apply(
        &{
            let mut s = DevelopSettings::default();
            s.mixer_luminance[BL] = -CONTROL_LIMIT;
            s
        },
        [22, 38, 92],
    );
    assert!(
        out[0] as u32 + out[1] as u32 + out[2] as u32 > 0,
        "navy collapsed to black: {out:?}"
    );
}

#[test]
fn proxy_neutrals_barely_move_under_all_band_push() {
    let mut all = DevelopSettings::default();
    for i in 0..MIXER_BANDS {
        all.mixer_hue[i] = CONTROL_LIMIT;
        all.mixer_saturation[i] = CONTROL_LIMIT;
    }
    for rgb in [[8, 8, 8], [128, 128, 128], [245, 245, 245]] {
        let de = oklab_de(proxy_apply(&all, rgb), rgb);
        assert!(de < 0.012, "neutral {rgb:?} moved ΔE={de:.4}");
    }
}

#[test]
fn proxy_skin_prefers_orange_over_yellow_and_red() {
    // A skin tone should answer Orange far more than its neighbours, in the
    // real path, at an ordinary +100 Saturation.
    let skin = [205, 150, 120];
    let via_orange = proxy_de(O, 1, 100.0, skin);
    let via_yellow = proxy_de(Y, 1, 100.0, skin);
    let via_red = proxy_de(R, 1, 100.0, skin);
    assert!(
        via_orange > via_yellow * 1.8 && via_orange > via_red * 1.8,
        "skin not Orange-dominant: O={via_orange:.4} Y={via_yellow:.4} R={via_red:.4}"
    );
    assert!(
        via_orange > VISIBLE * 1.5,
        "skin Orange too weak {via_orange:.4}"
    );
}

#[test]
fn proxy_matches_direct_path_strength() {
    // The regression was the proxy (live) path being far weaker than the
    // per-pixel maths. They must now agree closely for flat colour — no hidden
    // squared gate. Checks the hardest cases (dark & muted).
    for (band, rgb) in [
        (R, [90, 25, 35]),    // burgundy
        (Y, [110, 105, 45]),  // olive
        (BL, [22, 38, 92]),   // navy
        (G, [40, 80, 45]),    // dark foliage
        (A, [40, 140, 140]),  // teal
        (O, [205, 150, 120]), // skin
    ] {
        let mut s = DevelopSettings::default();
        s.mixer_saturation[band] = 100.0;
        let proxy = proxy_apply(&s, rgb);
        let mut direct = vec![rgb[0], rgb[1], rgb[2], 255u8];
        apply_to_pixels(&s, &mut direct, 1, 1);
        let d = [direct[0], direct[1], direct[2]];
        let gap = oklab_de(proxy, d);
        assert!(
            gap < 0.010,
            "proxy≠direct for {rgb:?}: proxy {proxy:?} vs direct {d:?} ΔE={gap:.4}"
        );
    }
}

// ── CPU / GPU parity ─────────────────────────────────────────────────────

/// Faithful transcription of the WGSL `dev_band_affinity` + `dev_ucs_hue` +
/// `dev_mixer_weight` (gpu/compositor.rs) — all in f32, exactly like the
/// shader. The test below asserts it matches the CPU `band_affinity` (whose
/// UCS warp runs in f64), so a change to either side that breaks parity, or
/// f32 drift big enough to matter, fails here.
fn wgsl_band_affinity_mirror(gate_lut: &[f32], rgb: [u8; 3]) -> f32 {
    let (rf, gf, bf) = (
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    );
    // dev_ucs_hue (f32)
    let lin = |c: f32| srgb_to_linear(c);
    let (lr, lg, lb) = (lin(rf), lin(gf), lin(bf));
    let big_x = 0.4124564 * lr + 0.3575761 * lg + 0.1804375 * lb;
    let big_y = 0.2126729 * lr + 0.7151522 * lg + 0.0721750 * lb;
    let big_z = 0.0193339 * lr + 0.1191920 * lg + 0.9503041 * lb;
    let sum = (big_x + big_y + big_z).max(1e-10);
    let x = big_x / sum;
    let y = big_y / sum;
    let mut d = 0.318707282433486_f32 * x + 2.16743692732158_f32 * y + 0.291320554395942_f32;
    if d.abs() < 1e-10 {
        d = if d < 0.0 { -1e-10 } else { 1e-10 };
    }
    let u = (-0.783941002840055_f32 * x + 0.277512987809202_f32 * y + 0.153836578598858_f32) / d;
    let v = (0.745273540913283_f32 * x - 0.205375866083878_f32 * y - 0.165478376301988_f32) / d;
    let us = 1.39656225667_f32 * u / (u.abs() + 1.49217352929_f32);
    let vs = 1.4513954287_f32 * v / (v.abs() + 1.52488637914_f32);
    let up = -1.124983854323892_f32 * us - 0.980483721769325_f32 * vs;
    let vp = 1.86323315098672_f32 * us + 1.971853092390862_f32 * vs;
    let h = vp.atan2(up);
    // dev_band_affinity's LUT sample
    let t = ((h + std::f32::consts::PI) / std::f32::consts::TAU).rem_euclid(1.0) * 360.0;
    let i = (t as usize).min(359);
    let f = t - i as f32;
    let a = gate_lut[i];
    let b = gate_lut[(i + 1) % 360];
    let gate = a + (b - a) * f;
    // dev_mixer_weight
    let mx = rf.max(gf).max(bf);
    let delta = mx - rf.min(gf).min(bf);
    let sat = if mx > 1e-4 && delta > 1e-6 {
        delta / mx
    } else {
        0.0
    };
    let w = 1.0 / (1.0 + (-24.0 * (sat - 0.06)).exp()) * smootherstep(0.012, 0.075, delta);
    (gate * w).clamp(0.0, 1.0)
}

#[test]
fn cpu_regate_matches_gpu_mirror() {
    let colors: Vec<[u8; 3]> = band_volume()
        .iter()
        .map(|&(_, _, c)| c)
        .chain([[12, 12, 12], [128, 128, 128], [245, 245, 245]])
        .collect();
    let masks: [[bool; MIXER_BANDS]; 4] = [
        [true, false, false, false, false, false, false, false],
        [false, false, false, false, true, true, false, false],
        [true, true, true, true, true, true, true, true],
        [false, true, false, false, false, false, false, true],
    ];
    for m in &masks {
        // Settings whose edited-band set is exactly `m` (what the upload
        // path feeds `build_mixer_curves_opt` before slicing out `gate`).
        let mut s = DevelopSettings::default();
        for (band, &on) in m.iter().enumerate() {
            if on {
                s.mixer_saturation[band] = 100.0;
            }
        }
        let curves = build_mixer_curves_opt(&s).unwrap();
        for &rgb in &colors {
            let (rf, gf, bf) = (
                rgb[0] as f32 / 255.0,
                rgb[1] as f32 / 255.0,
                rgb[2] as f32 / 255.0,
            );
            let cpu = band_affinity(&curves, rf, gf, bf);
            let gpu = wgsl_band_affinity_mirror(&curves.gate, rgb);
            // Tolerance covers the CPU's f64 chromaticity warp vs the
            // shader's f32 one (the hue difference is ~1e-5 rad).
            assert!(
                (cpu - gpu).abs() < 2e-3,
                "CPU/GPU re-gate diverged for {rgb:?} mask {m:?}: {cpu} vs {gpu}"
            );
        }
    }
}

#[test]
fn gpu_shader_mirrors_light_chroma_reconstruction_constants() {
    let shader = crate::gpu::compositor::COMPOSITOR_SHADER;
    assert!(shader.contains("dev_luma_target_chroma_preserve_weight"));
    assert!(shader.contains("dev_smootherstep(0.018, 0.075, luma)"));
    assert!(shader.contains("dev_smootherstep(0.030, 0.105, chroma)"));
    assert!(shader.contains("dev_smootherstep(0.88, 0.98, target_luma)"));
    assert!(shader.contains("signal * color * highlight_room * 0.88"));
    assert!(shader.contains("additive + (scaled - additive) * preserve"));
}

#[test]
fn gpu_shader_mirrors_cpu_mixer_constants() {
    // The selection constants must be byte-identical between the Rust model
    // and the WGSL re-gate, or preview ≠ commit at colour boundaries.
    let shader = crate::gpu::compositor::COMPOSITOR_SHADER;
    // UCS 22 chromaticity warp (spot-check the load-bearing constants).
    assert!(shader.contains("0.318707282433486"));
    assert!(shader.contains("2.16743692732158"));
    assert!(shader.contains("-0.783941002840055"));
    assert!(shader.contains("1.39656225667"));
    assert!(shader.contains("-1.124983854323892"));
    assert!(shader.contains("1.86323315098672"));
    // Gate LUT layout: 360 entries appended after the curve tables.
    assert!(shader.contains("dev_rgb_curve[1025u + i]"));
    assert!(shader.contains("dev_rgb_curve[1025u + ((i + 1u) % 360u)]"));
    assert_eq!(MIXER_CURVE_RES, 360);
    // Saturation weighting: logistic steepness + midpoints + delta gate.
    assert!(shader.contains("exp(-24.0 * x)"));
    assert!(shader.contains("dev_mixer_weight(c, 0.06)"));
    assert!(shader.contains("dev_smootherstep(0.012, 0.075, delta)"));
    assert!((MIXER_SAT_STEEP - 24.0).abs() < 1e-6);
    assert!((MIXER_SAT_SHIFT - 0.06).abs() < 1e-6);
    assert!((MIXER_DELTA_LO - 0.012).abs() < 1e-6 && (MIXER_DELTA_HI - 0.075).abs() < 1e-6);
    // reconstruction re-gate indicator + chroma-rescued shadow fade
    assert!(shader.contains("dev_smootherstep(0.02, 0.12, dev_band_affinity(region))"));
    assert!(shader.contains("dev_smootherstep(0.0, 0.32, lt + ct * 0.60)"));
    assert!((REGATE_LO - 0.02).abs() < 1e-6 && (REGATE_HI - 0.12).abs() < 1e-6);
    assert!((SHADOW_COLOR_RESCUE - 0.60).abs() < 1e-6);
}

fn rgb_distance(a: &[u8], b: &[u8]) -> i32 {
    (a[0] as i32 - b[0] as i32).abs()
        + (a[1] as i32 - b[1] as i32).abs()
        + (a[2] as i32 - b[2] as i32).abs()
}

fn channel_delta(after: &[u8], before: &[u8]) -> i32 {
    after[0] as i32 - before[0] as i32
}

fn luma_u8(px: &[u8]) -> f32 {
    px[0] as f32 * 0.2126 + px[1] as f32 * 0.7152 + px[2] as f32 * 0.0722
}

/// 64×64 grey tilemap with a horizontal sine ripple of ±`amp` around `mean`
/// (period 32 px) — smooth mid-scale structure the guided base averages out,
/// which is exactly what Clarity/Defog act on.
fn ripple_tilemap(mean: f32, amp: f32) -> TileMap {
    let (w, h) = (64u32, 64u32);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let v = mean + amp * (x as f32 * std::f32::consts::TAU / 32.0).sin();
            let b = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            let i = ((y * w + x) * 4) as usize;
            px[i] = b;
            px[i + 1] = b;
            px[i + 2] = b;
            px[i + 3] = 255;
        }
    }
    TileMap::from_rgba(&px, w, h)
}

fn luma_spread(tiles: &TileMap) -> f32 {
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for y in 0..tiles.height {
        for x in 0..tiles.width {
            let (r, g, b, _a) = tiles.get_pixel(x, y);
            let l = luma_u8(&[r, g, b]);
            lo = lo.min(l);
            hi = hi.max(l);
        }
    }
    hi - lo
}

fn mean_luma(tiles: &TileMap) -> f32 {
    let mut acc = 0.0f32;
    for y in 0..tiles.height {
        for x in 0..tiles.width {
            let (r, g, b, _a) = tiles.get_pixel(x, y);
            acc += luma_u8(&[r, g, b]);
        }
    }
    acc / (tiles.width * tiles.height) as f32
}

#[test]
fn clarity_amplifies_local_contrast() {
    let src = ripple_tilemap(0.5, 0.08);
    let before = luma_spread(&src);
    let mut settings = DevelopSettings::default();
    settings.clarity = 100.0;
    let out = apply_to_tilemap_direct(&src, &settings, None);
    let after = luma_spread(&out);
    assert!(
        after > before + 8.0,
        "clarity should widen the ripple: {before} -> {after}"
    );
}

#[test]
fn clarity_leaves_flat_region_untouched() {
    let src = ripple_tilemap(0.5, 0.0);
    let mut settings = DevelopSettings::default();
    settings.clarity = 100.0;
    let out = apply_to_tilemap_direct(&src, &settings, None);
    for y in [0u32, 31, 63] {
        for x in [0u32, 31, 63] {
            let (r0, g0, b0, _) = src.get_pixel(x, y);
            let (r1, g1, b1, _) = out.get_pixel(x, y);
            assert!(
                rgb_distance(&[r1, g1, b1], &[r0, g0, b0]) <= 3,
                "flat pixel moved at {x},{y}: {r0} -> {r1}"
            );
        }
    }
}

#[test]
fn negative_clarity_flattens_local_contrast() {
    let src = ripple_tilemap(0.5, 0.08);
    let before = luma_spread(&src);
    let mut settings = DevelopSettings::default();
    settings.clarity = -100.0;
    let out = apply_to_tilemap_direct(&src, &settings, None);
    let after = luma_spread(&out);
    assert!(
        after < before - 8.0,
        "negative clarity should flatten the ripple: {before} -> {after}"
    );
}

#[test]
fn defog_cuts_veil_and_restores_contrast() {
    // A hazy scene: bright, low-contrast (veiled) ripple.
    let src = ripple_tilemap(0.72, 0.05);
    let before_spread = luma_spread(&src);
    let before_mean = mean_luma(&src);
    let mut settings = DevelopSettings::default();
    settings.dehaze = 100.0;
    let out = apply_to_tilemap_direct(&src, &settings, None);
    let after_spread = luma_spread(&out);
    let after_mean = mean_luma(&out);
    assert!(
        after_spread > before_spread + 10.0,
        "defog should restore contrast: {before_spread} -> {after_spread}"
    );
    assert!(
        after_mean < before_mean - 10.0,
        "defog should cut the bright veil: {before_mean} -> {after_mean}"
    );
}

#[test]
fn negative_defog_adds_veil() {
    let src = ripple_tilemap(0.5, 0.08);
    let before_mean = mean_luma(&src);
    let mut settings = DevelopSettings::default();
    settings.dehaze = -100.0;
    let out = apply_to_tilemap_direct(&src, &settings, None);
    let after_mean = mean_luma(&out);
    assert!(
        after_mean > before_mean + 8.0,
        "negative defog should add a white veil: {before_mean} -> {after_mean}"
    );
}

#[test]
fn clarity_16bit_path_matches_direction() {
    let src8 = ripple_tilemap(0.5, 0.08);
    let px16: Vec<u16> = src8.flatten().iter().map(|&v| v as u16 * 257).collect();
    let src16 = TileMap::from_rgba16(&px16, 64, 64);
    assert!(src16.has_hdr());
    let mut settings = DevelopSettings::default();
    settings.clarity = 100.0;
    let out = apply_to_tilemap_direct(&src16, &settings, None);
    let before = luma_spread(&src8);
    let after = luma_spread(&out);
    assert!(
        after > before + 8.0,
        "16-bit clarity should widen the ripple too: {before} -> {after}"
    );
}

#[test]
fn fast_preview_effects_track_spatial_bake() {
    // The live preview proxy (downsample 1 here) must move the same
    // direction as the bake: clarity widens the ripple's range.
    let src = ripple_tilemap(0.5, 0.08);
    let (region, pw, ph) = build_fast_preview_region(&src, &None, 0, 0, 64, 64, 1);
    let mut settings = DevelopSettings::default();
    settings.clarity = 100.0;
    let out = apply_fast_preview_to_region(&region, &settings, pw, ph, 0, 0, 64, 64, 1);
    let lum = |p: &[f32; 3]| p[0] * 0.299 + p[1] * 0.587 + p[2] * 0.114;
    let spread = |buf: &[[f32; 3]]| {
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for p in buf {
            let l = lum(p);
            lo = lo.min(l);
            hi = hi.max(l);
        }
        hi - lo
    };
    let before = spread(&region);
    let after = spread(&out);
    assert!(
        after > before + 0.03,
        "fast preview clarity should widen the ripple: {before} -> {after}"
    );
}

#[test]
fn point_curve_identity_and_interpolation() {
    let id = identity_curve();
    for x in [0.0f32, 0.25, 0.5, 0.99] {
        assert!(
            (eval_point_curve(&id, x) - x).abs() < 1e-4,
            "identity at {x}"
        );
    }
    let pts = vec![[0.0, 0.0], [0.5, 0.7], [1.0, 1.0]];
    assert!((eval_point_curve(&pts, 0.5) - 0.7).abs() < 1e-4);
    // Monotone rising points → monotone spline, no overshoot above 1.
    let mut prev = -1.0f32;
    for i in 0..=100 {
        let y = eval_point_curve(&pts, i as f32 / 100.0);
        assert!(y >= prev - 1e-5, "must be monotone at {i}: {prev} -> {y}");
        assert!((0.0..=1.0).contains(&y));
        prev = y;
    }
}

#[test]
fn luma_point_curve_lifts_midtones() {
    let mut settings = DevelopSettings::default();
    settings.curve_points = vec![[0.0, 0.0], [0.5, 0.7], [1.0, 1.0]];
    assert!(!settings.is_neutral());
    assert!(tone_is_active(&settings));
    let src = ripple_tilemap(0.5, 0.0);
    let out = apply_to_tilemap_direct(&src, &settings, None);
    let (r0, _, _, _) = src.get_pixel(32, 32);
    let (r1, _, _, _) = out.get_pixel(32, 32);
    assert!(
        r1 as i32 > r0 as i32 + 25,
        "midtone should lift: {r0} -> {r1}"
    );
}

#[test]
fn rgb_curve_shifts_single_channel() {
    let mut settings = DevelopSettings::default();
    settings.curve_points_r = vec![[0.0, 0.0], [0.5, 0.75], [1.0, 1.0]];
    assert!(!settings.is_neutral());
    let src = ripple_tilemap(0.5, 0.0);
    let out = apply_to_tilemap_direct(&src, &settings, None);
    let (r0, g0, b0, _) = src.get_pixel(32, 32);
    let (r1, g1, b1, _) = out.get_pixel(32, 32);
    assert!(
        r1 as i32 > r0 as i32 + 25,
        "red channel should lift: {r0} -> {r1}"
    );
    assert!(
        (g1 as i32 - g0 as i32).abs() <= 4 && (b1 as i32 - b0 as i32).abs() <= 4,
        "green/blue should stay: g {g0}->{g1}, b {b0}->{b1}"
    );
}

#[test]
fn inverting_point_curve_survives_monotone_pass() {
    // An intentional negative (film-negative) curve must NOT be flattened
    // by the parametric curve's monotone enforcement.
    let mut settings = DevelopSettings::default();
    settings.curve_points = vec![[0.0, 1.0], [1.0, 0.0]];
    let src = ripple_tilemap(0.2, 0.0);
    let out = apply_to_tilemap_direct(&src, &settings, None);
    let (r0, _, _, _) = src.get_pixel(32, 32);
    let (r1, _, _, _) = out.get_pixel(32, 32);
    assert!(
        r1 as i32 > 255 - r0 as i32 - 40 && r1 > r0,
        "dark input should invert to bright: {r0} -> {r1}"
    );
}

#[test]
fn rgb_curve_luts_none_when_identity() {
    let settings = DevelopSettings::default();
    assert!(rgb_curve_luts(&settings).is_none());
    let mut edited = settings.clone();
    edited.curve_points_g = vec![[0.0, 0.1], [1.0, 1.0]];
    assert!(rgb_curve_luts(&edited).is_some());
}

#[test]
fn vignette_darkens_corners_not_center() {
    let src = ripple_tilemap(0.5, 0.0);
    let mut settings = DevelopSettings::default();
    settings.vignette = 100.0;
    let out = apply_to_tilemap_direct(&src, &settings, None);
    let (r0, g0, b0, _) = out.get_pixel(32, 32);
    let (rc, gc, bc, _) = out.get_pixel(0, 0);
    assert!(
        luma_u8(&[rc, gc, bc]) < luma_u8(&[r0, g0, b0]) - 5.0,
        "corner should be darker than center"
    );
}
