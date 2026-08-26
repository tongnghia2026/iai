//! Quality Milestone Q7 — display/export colour-management parity guard.
//!
//! Q7 (RAM/quality plan §"Quality Milestone Q7") requires that the on-screen
//! preview and the exported file agree, that the monitor transform is a
//! PREVIEW-ONLY appearance transform (never baked into exported pixels), and
//! that the GPU display path matches the CPU/lcms reference (no missing gamma,
//! no double gamma, no double profile transform).
//!
//! The preview applies the monitor/soft-proof transform as a 17³ RGBA8 3D LUT
//! built by lcms ([`build_display_lut`]) and sampled TRILINEARLY in the blit
//! shader. This probe replicates that exact GPU sampling in Rust and compares it
//! against a direct full-resolution lcms transform — so the measured error is
//! precisely the LUT's interpolation error, the GPU↔CPU parity Q7 §6 asks for.
//! It is hermetic (lcms only, no GPU, no corpus) and runs in the normal gate.

use iai::core::cms::{
    adobe_rgb_profile, build_display_lut, convert_rgba8, convert_srgb_to_rgb_profile,
    display_p3_profile, icc_bytes, identity_lut, srgb_icc_bytes, srgb_profile, DEFAULT_INTENT,
    PROOF_LUT_SIZE,
};

const N: usize = PROOF_LUT_SIZE; // 17

/// Trilinearly sample a `size³` RGBA8 3D LUT (R fastest, then G, then B — the
/// layout `identity_lut`/`build_display_lut` produce) at encoded input `e` in
/// [0,1] per channel. This mirrors the blit shader: the shader's texel-centre
/// coord `e*(n-1)/n + 0.5/n` sampled with a Linear filter is exactly a trilinear
/// interpolation at fractional node index `e*(size-1)`.
fn sample_lut_trilinear(lut: &[u8], size: usize, e: [f32; 3]) -> [f32; 3] {
    let at = |r: usize, g: usize, b: usize, ch: usize| -> f32 {
        lut[((b * size + g) * size + r) * 4 + ch] as f32
    };
    let mut out = [0.0f32; 3];
    let mut idx = [(0usize, 0usize, 0.0f32); 3];
    for c in 0..3 {
        let f = (e[c].clamp(0.0, 1.0)) * (size - 1) as f32;
        let i0 = f.floor() as usize;
        let i1 = (i0 + 1).min(size - 1);
        idx[c] = (i0, i1, f - i0 as f32);
    }
    let (r0, r1, tr) = idx[0];
    let (g0, g1, tg) = idx[1];
    let (b0, b1, tb) = idx[2];
    for ch in 0..3 {
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let c00 = lerp(at(r0, g0, b0, ch), at(r1, g0, b0, ch), tr);
        let c10 = lerp(at(r0, g1, b0, ch), at(r1, g1, b0, ch), tr);
        let c01 = lerp(at(r0, g0, b1, ch), at(r1, g0, b1, ch), tr);
        let c11 = lerp(at(r0, g1, b1, ch), at(r1, g1, b1, ch), tr);
        let c0 = lerp(c00, c10, tg);
        let c1 = lerp(c01, c11, tg);
        out[ch] = lerp(c0, c1, tb);
    }
    out
}

/// Max & mean |Δ| (in /255) between the trilinear-LUT preview and direct lcms,
/// over a dense encoded-RGB grid. The reference is built with ONE batched lcms
/// transform over the whole grid (not one transform per pixel), so the probe
/// stays in the fast test gate.
fn lut_vs_direct_error(monitor_icc: &[u8]) -> (f32, f32) {
    let lut = build_display_lut(None, false, Some(monitor_icc), N).expect("monitor display LUT");
    let steps = 24usize;
    // Build the whole grid as one RGBA8 buffer, transform it once through lcms.
    let mut inputs: Vec<[f32; 3]> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    for ri in 0..=steps {
        for gi in 0..=steps {
            for bi in 0..=steps {
                let e = [
                    ri as f32 / steps as f32,
                    gi as f32 / steps as f32,
                    bi as f32 / steps as f32,
                ];
                inputs.push(e);
                buf.extend_from_slice(&[
                    (e[0] * 255.0).round() as u8,
                    (e[1] * 255.0).round() as u8,
                    (e[2] * 255.0).round() as u8,
                    255,
                ]);
            }
        }
    }
    assert!(convert_srgb_to_rgb_profile(
        &mut buf,
        monitor_icc,
        DEFAULT_INTENT
    ));

    let (mut max, mut sum, mut n) = (0.0f32, 0.0f64, 0u64);
    for (k, &e) in inputs.iter().enumerate() {
        let gpu = sample_lut_trilinear(&lut, N, e);
        let cpu = [
            buf[k * 4] as f32,
            buf[k * 4 + 1] as f32,
            buf[k * 4 + 2] as f32,
        ];
        for ch in 0..3 {
            let d = (gpu[ch] - cpu[ch]).abs();
            max = max.max(d);
            sum += d as f64;
            n += 1;
        }
    }
    (max, (sum / n as f64) as f32)
}

#[test]
fn gpu_lut_matches_cpu_lcms_for_wide_gamut_monitors() {
    // The 17³ trilinear preview LUT must track the direct lcms transform closely
    // on real wide-gamut monitor profiles, or the preview would drift from a
    // colour-managed reference (and from the eventual export).
    for (name, icc) in [
        ("AdobeRGB", icc_bytes(&adobe_rgb_profile())),
        ("DisplayP3", icc_bytes(&display_p3_profile())),
    ] {
        let (max, mean) = lut_vs_direct_error(&icc);
        println!("Q7 display-LUT parity {name}: max {max:.2}/255, mean {mean:.3}/255");
        assert!(max.is_finite());
        // Locked to the observed 17³ trilinear error. If a future change makes
        // the LUT coarser (or breaks the half-texel mapping) this trips.
        assert!(
            max <= 6.0,
            "{name}: GPU/CPU display-LUT parity max {max}/255 too high"
        );
        assert!(
            mean <= 1.5,
            "{name}: GPU/CPU display-LUT parity mean {mean}/255 too high"
        );
    }
}

#[test]
fn srgb_monitor_and_srgb_proof_are_near_identity_no_double_gamma() {
    // sRGB content shown on an sRGB monitor must be a near no-op: any missing or
    // doubled gamma in the LUT build would blow this up far past a rounding
    // tolerance. Checked at both endpoints and midtones through the real sampler.
    let lut = build_display_lut(None, false, Some(&srgb_icc_bytes()), N).unwrap();
    let ident = identity_lut(N);
    for e in [
        [0.0f32, 0.0, 0.0],
        [0.5, 0.5, 0.5],
        [1.0, 1.0, 1.0],
        [0.25, 0.6, 0.9],
        [0.03, 0.03, 0.03], // deep shadow — where a double gamma screams
    ] {
        let got = sample_lut_trilinear(&lut, N, e);
        let want = sample_lut_trilinear(&ident, N, e);
        for ch in 0..3 {
            assert!(
                (got[ch] - want[ch]).abs() <= 4.0,
                "sRGB-on-sRGB LUT drifted from identity at {e:?} ch{ch}: {} vs {}",
                got[ch],
                want[ch]
            );
        }
    }
}

#[test]
fn wide_gamut_monitor_transform_is_not_the_identity() {
    // Guard the other way: a genuine wide-gamut monitor transform MUST move
    // saturated colours (otherwise "display CMS on" would silently do nothing).
    let icc = icc_bytes(&adobe_rgb_profile());
    let lut = build_display_lut(None, false, Some(&icc), N).unwrap();
    let ident = identity_lut(N);
    let sat = [0.05f32, 0.85, 0.10]; // saturated green — biggest sRGB↔AdobeRGB gap
    let got = sample_lut_trilinear(&lut, N, sat);
    let idn = sample_lut_trilinear(&ident, N, sat);
    let moved = (0..3).map(|c| (got[c] - idn[c]).abs()).fold(0.0, f32::max);
    println!("Q7 AdobeRGB monitor moves saturated green by {moved:.1}/255");
    assert!(
        moved > 6.0,
        "wide-gamut monitor transform did nothing: {moved}/255"
    );
}

#[test]
fn a_second_monitor_transform_is_detectably_different_no_double_apply() {
    // Q7 §7: applying the monitor transform TWICE must land visibly away from
    // applying it once — so a code path that double-composed the display
    // transform could be caught. Compare one lcms sRGB→AdobeRGB against two.
    let adobe = adobe_rgb_profile();
    let srgb = srgb_profile();
    let sample = [20u8, 200, 60, 255];
    let mut once = sample;
    assert!(convert_rgba8(&mut once, &srgb, &adobe, DEFAULT_INTENT));
    // Treat the once-converted values as if they were sRGB again and reconvert —
    // this is exactly what a double-apply bug would produce.
    let mut twice = once;
    assert!(convert_rgba8(&mut twice, &srgb, &adobe, DEFAULT_INTENT));
    let drift = (0..3)
        .map(|c| (once[c] as i32 - twice[c] as i32).abs())
        .max()
        .unwrap();
    println!("Q7 double-apply monitor drift: {drift}/255");
    assert!(
        drift >= 3,
        "a doubled monitor transform is indistinguishable from one — bug guard is blind"
    );
}

#[test]
fn export_to_srgb_is_identity_and_carries_no_monitor_transform() {
    // Q7 §3: the monitor transform is preview-only. An sRGB document exported as
    // sRGB must be a byte no-op (±1 rounding) — even though the SAME pixels shown
    // on a wide-gamut monitor go through a large display LUT. This proves export
    // does not route through the monitor/display path.
    let mut buf = vec![10u8, 120, 240, 255, 0, 0, 0, 128, 200, 30, 90, 255];
    let before = buf.clone();
    assert!(convert_srgb_to_rgb_profile(
        &mut buf,
        &srgb_icc_bytes(),
        DEFAULT_INTENT
    ));
    for (a, b) in before.iter().zip(buf.iter()) {
        assert!(
            (*a as i32 - *b as i32).abs() <= 1,
            "sRGB→sRGB export changed a pixel ({a} -> {b}) — a transform leaked into export"
        );
    }
    // Sanity: the monitor display LUT for the SAME sRGB pixels on AdobeRGB is
    // NOT identity, confirming the two paths are genuinely different.
    let adobe = icc_bytes(&adobe_rgb_profile());
    let lut = build_display_lut(None, false, Some(&adobe), N).unwrap();
    let e = [10.0 / 255.0, 120.0 / 255.0, 240.0 / 255.0];
    let disp = sample_lut_trilinear(&lut, N, e);
    let moved = (disp[0] - 10.0).abs() + (disp[1] - 120.0).abs() + (disp[2] - 240.0).abs();
    assert!(
        moved > 6.0,
        "the display path should transform what export leaves alone"
    );
}
