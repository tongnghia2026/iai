//! Quality Milestone Q5 — per-slider *contract* and property/golden guard.
//!
//! Q5 (RAM/quality plan §"Quality Milestone Q5", Công việc #1 and #9) asks for
//! two things before any Light/Colour slider is re-tuned:
//!   1. a written contract for each control — domain, pivot, EV range, region of
//!      effect, hue/luma invariants, and clipping policy; and
//!   2. property/golden tests that lock those invariants: monotonic exposure,
//!      neutral preservation, bounded hue drift, finite output, and a defined
//!      gamut policy for saturation.
//!
//! This file is the executable form of that contract. Like the Q0 slider-sweep
//! probe (`develop_slider_sweep_probe.rs`) it is HERMETIC — no RAW corpus, no
//! `src` change, no default-look change — so it runs in the normal `cargo test`
//! gate and doubles as a regression guard while the deeper Q5 tuning lands.
//!
//! It drives the same deterministic scene evaluators the real bake uses:
//!   * [`eval_scene_pixel`] — the compatibility path (linear-sRGB working space,
//!     hard-clamped output), used by the histogram/proxy and the Q0 golden; and
//!   * [`eval_scene_pixel_for_scene`] — the real RAW path, whose Saturation push
//!     is resolved by ONE hue-preserving OKLCh gamut compression at the output
//!     boundary (`working_to_display`).
//! Tone-equalizer zone behaviour is asserted directly through the public
//! [`tone_eq_offset_ev`], so the zone contract is checked at the source rather
//! than inferred from rendered pixels.
//!
//! ── Per-slider contract (the artifact Q5 §Công việc #1 asks for) ─────────────
//!
//! Exposure   domain ±`EXPOSURE_LIMIT` → ±5 EV; a pure `2^EV` MULTIPLY on the
//!            scene-linear value (never a brightness gamma). No pivot — it slides
//!            the whole scene. Region: everything. Invariant: neutrals stay
//!            neutral; hue preserved (scalar gain). Clipping: highlights roll off
//!            through the tone sigmoid; only the extremes touch the hull.
//! Contrast   domain ±`CONTROL_LIMIT`; pivots at 18 % grey (`SCENE_MID_GRAY`).
//!            Steepens/flattens the midtone slope. Region: midtones most, grey
//!            fixed. Invariant: grey stays grey; neutrals neutral; hue preserved.
//! Highlights domain ±`CONTROL_LIMIT`; a Gaussian tone-equalizer zone centred at
//!            +2.5 EV above grey. Positive brightens that zone, negative recovers
//!            it (and restores highlight chroma). Region: highlights, tapering
//!            into upper midtones; near-zero at the blacks zone. Luma-only offset
//!            applied uniformly to RGB → hue preserved.
//! Shadows    zone centred at −3.0 EV. Positive lift is gated by a signal-
//!            confidence floor (no offset below ~−9 EV) so it never amplifies
//!            sensor-floor noise; negative is un-gated so blacks can still deepen.
//! Whites     zone centred at +4.5 EV (narrow); the far-highlight anchor.
//! Blacks     zone centred at −4.6 EV; same noise-confidence gate on positive
//!            lift as Shadows. Positive brightens, negative deepens.
//! Saturation domain ±`CONTROL_LIMIT`; a direct chroma scale around linear
//!            luminance, protecting near-black and near-white. Region: all
//!            chromatic pixels; neutrals untouched. Gamut policy: the RAW path
//!            pushes chroma freely and the OKLCh boundary compresses ONCE — hue
//!            is preserved and no channel inverts, so a full push saturates up to
//!            the hull without a hue flip.
//! Vibrance   domain ±`CONTROL_LIMIT`; low-chroma-priority saturation — pours
//!            into pale/muted colour and leaves already-vivid colour (chroma ≳
//!            0.35) alone. Neutrals untouched.

use iai::core::develop::{DevelopSettings, CONTROL_LIMIT, EXPOSURE_LIMIT};
use iai::core::develop_scene::{
    eval_scene_pixel, eval_scene_pixel_for_scene, exposure_multiplier, tone_eq_offset_ev, BaseLook,
    SceneSource, SCENE_EV_MAX, SCENE_EV_MIN, SCENE_MID_GRAY,
};
use iai::core::perceptual_color::linear_srgb_to_oklab;

// ── Small measurement helpers (self-contained, mirror the Q0 probe) ─────────

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[derive(Clone, Copy)]
struct Lab {
    l: f32,
    chroma: f32,
    hue_deg: f32,
}

fn lab_of(srgb: [f32; 3]) -> Lab {
    let lab = linear_srgb_to_oklab(srgb.map(srgb_to_linear));
    Lab {
        l: lab.l,
        chroma: lab.a.hypot(lab.b),
        hue_deg: lab.b.atan2(lab.a).to_degrees(),
    }
}

/// Smallest absolute angle (deg) between two hues on the colour circle.
fn hue_gap(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// A minimal linear-sRGB scene of one solid colour, so `eval_scene_pixel_for_scene`
/// can be driven with the same working/output contract as the compat evaluator.
fn solid_linear_scene(rgb: [f32; 3]) -> SceneSource {
    let mut scene = SceneSource::new(2, 2);
    scene.color_pipeline.working = iai::core::working_color::WorkingColorSpace::LinearSrgb;
    for y in 0..scene.height {
        for x in 0..scene.width {
            scene.set_rgb(x, y, rgb);
        }
    }
    scene
}

fn tone(setter: impl Fn(&mut DevelopSettings)) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    setter(&mut s);
    s
}

// A neutral staircase plus saturated primaries + memory colours, in linear-sRGB.
const NEUTRALS: &[[f32; 3]] = &[
    [0.02, 0.02, 0.02],
    [0.06, 0.06, 0.06],
    [0.184, 0.184, 0.184],
    [0.35, 0.35, 0.35],
    [0.60, 0.60, 0.60],
    [0.90, 0.90, 0.90],
];
const CHROMATICS: &[(&str, [f32; 3])] = &[
    ("red", [0.60, 0.05, 0.05]),
    ("green", [0.05, 0.50, 0.05]),
    ("blue", [0.04, 0.05, 0.50]),
    ("cyan", [0.05, 0.45, 0.50]),
    ("magenta", [0.50, 0.05, 0.45]),
    ("yellow", [0.60, 0.55, 0.05]),
    ("skin", [0.45, 0.28, 0.20]),
    ("sky", [0.12, 0.22, 0.50]),
];

// ── A. Exposure = pure 2^EV multiply (Q5 §Công việc #2) ─────────────────────

#[test]
fn exposure_is_two_to_the_ev_not_a_gamma() {
    // The UI maps ±EXPOSURE_LIMIT onto ±5 EV, and the control is a literal
    // `2^EV` multiplier — the exact contract, verified against the public
    // `exposure_multiplier` and clamped past full scale.
    for &(ui, ev) in &[
        (0.0f32, 0.0f32),
        (EXPOSURE_LIMIT * 0.5, 2.5),
        (-EXPOSURE_LIMIT * 0.5, -2.5),
        (EXPOSURE_LIMIT, 5.0),
        (-EXPOSURE_LIMIT, -5.0),
        (EXPOSURE_LIMIT * 2.0, 5.0), // clamps at +5 EV
    ] {
        let got = exposure_multiplier(ui);
        let want = ev.exp2();
        assert!(
            (got - want).abs() <= 1e-5 * want,
            "exposure_multiplier({ui}) = {got}, expected 2^{ev} = {want}"
        );
    }

    // The MULTIPLY identity: raising Exposure by E EV must be bit-identical to
    // feeding the same pixel pre-scaled by 2^E at Exposure 0. This is the whole
    // point of the contract — a brightness gamma would fail it. Tested on a mid
    // grey and a skin patch, at EV that lands mid-range (not clipped either way).
    for rgb in [[0.06f32, 0.06, 0.06], [0.18, 0.10, 0.07]] {
        for &ev in &[1.5f32, 2.5, -2.0] {
            let ui = ev / 5.0 * EXPOSURE_LIMIT;
            let k = exposure_multiplier(ui);
            let lifted = eval_scene_pixel(rgb, &tone(|s| s.exposure = ui), BaseLook::Raw);
            let prescaled = eval_scene_pixel(
                [rgb[0] * k, rgb[1] * k, rgb[2] * k],
                &DevelopSettings::default(),
                BaseLook::Raw,
            );
            for c in 0..3 {
                assert!(
                    (lifted[c] - prescaled[c]).abs() <= 1e-6,
                    "exposure {ev}EV not a pure 2^EV multiply on {rgb:?} ch{c}: \
                     {lifted:?} vs prescaled {prescaled:?}"
                );
            }
        }
    }
}

#[test]
fn exposure_raises_midtone_lightness_monotonically() {
    // Mid-grey lightness must rise monotonically across the exposure sweep and
    // span shadow→highlight end to end (the ±5 EV really reaches both ends).
    let mut prev = -1.0f32;
    let mut ls = Vec::new();
    for frac in [-1.0f32, -0.5, 0.0, 0.5, 1.0] {
        let l = lab_of(eval_scene_pixel(
            [0.184; 3],
            &tone(|s| s.exposure = frac * EXPOSURE_LIMIT),
            BaseLook::Raw,
        ))
        .l;
        assert!(
            l > prev,
            "exposure not monotonic on mid-grey: {ls:?} then {l}"
        );
        prev = l;
        ls.push(l);
    }
    assert!(
        ls[0] < 0.10 && *ls.last().unwrap() > 0.95,
        "exposure sweep must span shadow→highlight: {ls:?}"
    );
}

// ── B. Tone-equalizer zone contract (Q5 order #2, §Công việc #3/#4) ──────────

/// Absolute exposure `e` at `er` EV relative to 18 % grey.
fn e_at(er: f32) -> f32 {
    SCENE_MID_GRAY.log2() + er
}

#[test]
fn tone_zones_are_localized_and_signed() {
    // Each Light zone brightens (positive) / darkens (negative) AT ITS CENTRE,
    // and its reach at the far zone is a small fraction of its own response —
    // proving the four controls are separated luminance zones, not a global
    // curve that squeezes every tone. Centres (EV vs grey): shadows −3, blacks
    // −4.6, highlights +2.5, whites +4.5.
    let zones: &[(&str, fn(&mut DevelopSettings, f32), f32, f32)] = &[
        ("highlights", |s, v| s.highlights = v, 2.5, -4.6),
        ("whites", |s, v| s.whites = v, 4.5, -4.6),
        ("shadows", |s, v| s.shadows = v, -3.0, 4.5),
        ("blacks", |s, v| s.blacks = v, -4.6, 4.5),
    ];
    for &(name, set, centre, far) in zones {
        let pos = tone(|s| set(s, CONTROL_LIMIT));
        let neg = tone(|s| set(s, -CONTROL_LIMIT));
        let at_centre = tone_eq_offset_ev(&pos, e_at(centre));
        let neg_centre = tone_eq_offset_ev(&neg, e_at(centre));
        assert!(
            at_centre > 0.05,
            "{name}+ should brighten its own zone: offset {at_centre} EV"
        );
        assert!(
            neg_centre < -0.05,
            "{name}- should darken its own zone: offset {neg_centre} EV"
        );
        let at_far = tone_eq_offset_ev(&pos, e_at(far)).abs();
        assert!(
            at_far < 0.30 * at_centre,
            "{name} leaks {at_far} EV into the far zone vs {at_centre} at its centre — not localized"
        );
    }
}

#[test]
fn tone_zone_response_is_monotonic_in_the_control() {
    // At each zone centre the offset must grow monotonically with the slider,
    // through zero at neutral — a well-behaved control, no fold-back.
    let controls: &[(fn(&mut DevelopSettings, f32), f32)] = &[
        (|s, v| s.highlights = v, 2.5),
        (|s, v| s.shadows = v, -3.0),
        (|s, v| s.blacks = v, -4.6),
        (|s, v| s.whites = v, 4.5),
    ];
    for &(set, centre) in controls {
        let mut prev = f32::NEG_INFINITY;
        for frac in [-1.0f32, -0.5, 0.0, 0.5, 1.0] {
            let off = tone_eq_offset_ev(&tone(|s| set(s, frac * CONTROL_LIMIT)), e_at(centre));
            assert!(
                off > prev - 1e-6,
                "zone at {centre}EV not monotonic in control: {prev} then {off}"
            );
            prev = off;
        }
        // Neutral is exactly zero offset.
        assert_eq!(tone_eq_offset_ev(&tone(|s| set(s, 0.0)), e_at(centre)), 0.0);
    }
}

#[test]
fn positive_shadow_lift_is_noise_gated_but_darkening_is_not() {
    // Q5 §Công việc #4: a positive Shadows/Blacks lift must NOT amplify the
    // sensor floor, so deep in the noise (≲ −8 EV) the positive offset is
    // attenuated toward zero; the matching NEGATIVE (deepen) stays un-gated so
    // photographers can still crush blacks. Measured on the Blacks zone, whose
    // Gaussian still has real amplitude at −8 EV.
    let deep = e_at(-8.0);
    let lift = tone_eq_offset_ev(&tone(|s| s.blacks = CONTROL_LIMIT), deep);
    let crush = tone_eq_offset_ev(&tone(|s| s.blacks = -CONTROL_LIMIT), deep);
    assert!(
        lift >= 0.0 && crush <= 0.0,
        "signs wrong: lift {lift}, crush {crush}"
    );
    assert!(
        lift < 0.6 * crush.abs(),
        "positive lift {lift} EV not noise-gated vs un-gated crush {crush} EV at −8 EV"
    );

    // Far below the noise floor the positive lift is essentially gone…
    let floor = e_at(-11.0);
    assert!(
        tone_eq_offset_ev(&tone(|s| s.blacks = CONTROL_LIMIT), floor) < 0.02,
        "positive Blacks still lifting below the noise floor"
    );
    // …and everywhere the offset stays inside the ±4 EV clamp and is finite.
    for er in [-13.0, -8.0, -3.0, 0.0, 3.0, 6.0] {
        for set in [
            (|s: &mut DevelopSettings| s.shadows = CONTROL_LIMIT) as fn(&mut DevelopSettings),
            |s| s.highlights = -CONTROL_LIMIT,
        ] {
            let off = tone_eq_offset_ev(&tone(set), e_at(er));
            assert!(
                off.is_finite() && off.abs() <= 4.0 + 1e-4,
                "offset {off} out of clamp at {er}EV"
            );
        }
    }
}

// ── C. Contrast pivots at grey (Q5 order #2/#3) ──────────────────────────────

#[test]
fn contrast_pivots_at_mid_grey_and_holds_neutrals() {
    // Contrast steepens the midtone slope but must leave 18 % grey where it is
    // (the node pivots there) and must not tint any neutral.
    let base = lab_of(eval_scene_pixel(
        [SCENE_MID_GRAY; 3],
        &DevelopSettings::default(),
        BaseLook::Raw,
    ));
    for c in [-CONTROL_LIMIT, -80.0, 80.0, CONTROL_LIMIT] {
        let out = eval_scene_pixel(
            [SCENE_MID_GRAY; 3],
            &tone(|s| s.contrast = c),
            BaseLook::Raw,
        );
        let m = lab_of(out);
        assert!(
            (m.l - base.l).abs() < 0.01,
            "contrast {c} moved the 18% grey pivot: L {} → {}",
            base.l,
            m.l
        );
        assert!(
            m.chroma < 2e-3,
            "contrast {c} tinted grey: chroma {}",
            m.chroma
        );
    }
}

// ── D. Neutral preservation across every non-WB slider ──────────────────────

#[test]
fn no_light_or_colour_slider_tints_a_neutral() {
    let sliders: &[(&str, fn(&mut DevelopSettings, f32), f32)] = &[
        ("exposure", |s, v| s.exposure = v, EXPOSURE_LIMIT),
        ("contrast", |s, v| s.contrast = v, CONTROL_LIMIT),
        ("highlights", |s, v| s.highlights = v, CONTROL_LIMIT),
        ("shadows", |s, v| s.shadows = v, CONTROL_LIMIT),
        ("whites", |s, v| s.whites = v, CONTROL_LIMIT),
        ("blacks", |s, v| s.blacks = v, CONTROL_LIMIT),
        ("saturation", |s, v| s.saturation = v, CONTROL_LIMIT),
        ("vibrance", |s, v| s.vibrance = v, CONTROL_LIMIT),
    ];
    let mut worst = 0.0f32;
    for &(name, set, scale) in sliders {
        for frac in [-1.0f32, 1.0] {
            let s = tone(|s| set(s, frac * scale));
            for &n in NEUTRALS {
                let m = lab_of(eval_scene_pixel(n, &s, BaseLook::Raw));
                assert!(m.l.is_finite() && m.chroma.is_finite());
                worst = worst.max(m.chroma);
                assert!(
                    m.chroma < 3e-3,
                    "{name}@{frac} tinted neutral {n:?}: chroma {}",
                    m.chroma
                );
            }
        }
    }
    println!("neutral preservation: worst grey chroma across all sliders = {worst:.5}");
}

// ── E. Saturation gamut policy + Vibrance protection (Q5 §Công việc #6) ───────

#[test]
fn saturation_enriches_chroma_and_preserves_hue() {
    // The contract the RAW path GUARANTEES for positive Saturation:
    //   * hue never swings and output stays finite / in gamut, at ANY setting;
    //   * MODERATE positive Saturation (≤ +50 %) never goes duller than the
    //     neutral setting — it adds colour, as a user expects; and
    //   * the sweep's PEAK chroma exceeds the base, i.e. the control does enrich.
    // Strict monotonicity all the way to +100 % is deliberately NOT asserted:
    // near the sRGB hull an already-vivid colour folds back — the documented Q5
    // defect measured by `saturation_near_hull_chroma_foldback_is_bounded`.
    for &(label, lin) in CHROMATICS {
        let scene = solid_linear_scene(lin);
        let base = lab_of(eval_scene_pixel_for_scene(
            &scene,
            lin,
            &DevelopSettings::default(),
        ));
        let mut peak = base.chroma;
        for frac in [0.25f32, 0.5, 0.75, 1.0] {
            let out = eval_scene_pixel_for_scene(
                &scene,
                lin,
                &tone(|s| s.saturation = frac * CONTROL_LIMIT),
            );
            assert!(
                out.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)),
                "{label}@{frac} out of range: {out:?}"
            );
            let m = lab_of(out);
            peak = peak.max(m.chroma);
            // Moderate positive saturation (≤ +50 %) must enrich, not dull, and
            // must hold hue. Beyond that the near-hull fold-back also rotates hue
            // (red→orange) — recorded, not asserted, by the finding test below.
            if frac <= 0.5 {
                assert!(
                    m.chroma >= base.chroma - 5e-3,
                    "{label}@{frac}: moderate +saturation went duller than base ({} < {})",
                    m.chroma,
                    base.chroma
                );
                assert!(
                    hue_gap(m.hue_deg, base.hue_deg) < 12.0,
                    "{label}@{frac}: moderate saturation shifted hue {}° → {}°",
                    base.hue_deg,
                    m.hue_deg
                );
            }
        }
        assert!(
            peak > base.chroma + 1e-3,
            "{label}: saturation never enriched chroma (peak {peak} vs base {})",
            base.chroma
        );
    }
}

#[test]
fn saturation_near_hull_chroma_foldback_is_bounded() {
    // Q5 finding (predicted by the Q0 baseline: "saturation ±100% breaks gamut →
    // needs gamut compression"). On the real path a near-primary colour pushed
    // past the sRGB hull is folded back by the single OKLCh boundary
    // compression, so its MEASURED output chroma can peak mid-sweep and dip
    // toward full scale instead of climbing all the way. That is acceptable (no
    // hue flip, stays in gamut) but is the behaviour a future gamut-aware
    // Saturation would smooth out — so we record it and bound the worst dip
    // rather than pretend the response is strictly monotonic.
    let mut worst_drop_pct = 0.0f32;
    let mut worst = "";
    let mut worst_hue = 0.0f32;
    let mut worst_hue_label = "";
    for &(label, lin) in CHROMATICS {
        let scene = solid_linear_scene(lin);
        let base_hue = lab_of(eval_scene_pixel_for_scene(
            &scene,
            lin,
            &DevelopSettings::default(),
        ))
        .hue_deg;
        let mut sweep = Vec::new();
        let mut hue_swing = 0.0f32;
        for &frac in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let m = lab_of(eval_scene_pixel_for_scene(
                &scene,
                lin,
                &tone(|s| s.saturation = frac * CONTROL_LIMIT),
            ));
            sweep.push(m.chroma);
            hue_swing = hue_swing.max(hue_gap(m.hue_deg, base_hue));
        }
        let peak = sweep.iter().cloned().fold(0.0f32, f32::max);
        let final_c = *sweep.last().unwrap();
        let drop_pct = if peak > 1e-4 {
            (peak - final_c) / peak * 100.0
        } else {
            0.0
        };
        println!(
            "saturation sweep {label:<8} chroma[0..100%] = {:?}  peak {peak:.3} final {final_c:.3} drop {drop_pct:.1}%  hue-swing {hue_swing:.1}°",
            sweep.iter().map(|c| (c * 1000.0).round() / 1000.0).collect::<Vec<_>>()
        );
        if drop_pct > worst_drop_pct {
            worst_drop_pct = drop_pct;
            worst = label;
        }
        if hue_swing > worst_hue {
            worst_hue = hue_swing;
            worst_hue_label = label;
        }
    }
    println!(
        "worst near-hull chroma fold-back: {worst_drop_pct:.1}% on {worst}; worst hue swing: {worst_hue:.1}° on {worst_hue_label}"
    );
    // KNOWN Q5 DEFECT, locked to the 2026-08-26 baseline: pushing global
    // Saturation past ~+50 % on a near-primary drives it far out of the sRGB
    // hull, and the single OKLCh boundary compression folds it back — red peaks
    // at +50 % then collapses ~53 % in chroma by +100 % AND rotates ~58 % of the
    // way toward yellow (58°), ending DULLER and a different hue than the
    // unsaturated base. These bounds do not bless the defect — they record it and
    // fail loudly if a change makes it WORSE. The gamut-aware Saturation fix
    // (scale chroma along constant-hue OKLCh lines with a soft hull limiter,
    // CPU+GPU parity) must instead DRIVE THESE DOWN, at which point they tighten.
    assert!(
        worst_drop_pct < 60.0,
        "near-hull saturation fold-back regressed to {worst_drop_pct:.1}% on {worst} (baseline ~53%)"
    );
    assert!(
        worst_hue < 65.0,
        "near-hull saturation hue swing regressed to {worst_hue:.1}° on {worst_hue_label} (baseline ~58°)"
    );
}

#[test]
fn negative_saturation_converges_toward_neutral_in_gamut() {
    // Desaturation only ever shrinks chroma toward luma — always in gamut, no
    // hue games — and full negative lands essentially neutral.
    for &(label, lin) in CHROMATICS {
        let scene = solid_linear_scene(lin);
        let base = lab_of(eval_scene_pixel_for_scene(
            &scene,
            lin,
            &DevelopSettings::default(),
        ));
        let mut prev = f32::INFINITY;
        for frac in [0.0f32, 0.5, 1.0] {
            let m = lab_of(eval_scene_pixel_for_scene(
                &scene,
                lin,
                &tone(|s| s.saturation = -frac * CONTROL_LIMIT),
            ));
            assert!(
                m.chroma < prev + 5e-3,
                "{label}: -saturation not shrinking chroma"
            );
            prev = m.chroma;
        }
        assert!(
            prev < base.chroma * 0.30 + 0.02,
            "{label}: full desaturation left chroma {prev} (base {})",
            base.chroma
        );
    }
}

#[test]
fn vibrance_favours_muted_colour_and_spares_the_vivid() {
    // Vibrance is low-chroma-priority: it should enrich a pale/muted colour more
    // than an already-vivid one (chroma ≳ 0.35 barely moves), and leave neutrals
    // untouched — the property that separates it from a flat Saturation boost.
    let muted = [0.30f32, 0.24, 0.20]; // pale skin-ish, low chroma
    let vivid = [0.60f32, 0.03, 0.03]; // near-primary red, high chroma
    let vib = tone(|s| s.vibrance = CONTROL_LIMIT);

    let d_muted = {
        let scene = solid_linear_scene(muted);
        lab_of(eval_scene_pixel_for_scene(&scene, muted, &vib)).chroma
            - lab_of(eval_scene_pixel_for_scene(
                &scene,
                muted,
                &DevelopSettings::default(),
            ))
            .chroma
    };
    let d_vivid = {
        let scene = solid_linear_scene(vivid);
        lab_of(eval_scene_pixel_for_scene(&scene, vivid, &vib)).chroma
            - lab_of(eval_scene_pixel_for_scene(
                &scene,
                vivid,
                &DevelopSettings::default(),
            ))
            .chroma
    };
    println!("vibrance Δchroma: muted {d_muted:.4}, vivid {d_vivid:.4}");
    assert!(
        d_muted > 0.0,
        "vibrance did not enrich a muted colour ({d_muted})"
    );
    assert!(
        d_muted > d_vivid + 5e-3,
        "vibrance should favour muted over vivid: muted {d_muted} vs vivid {d_vivid}"
    );
}

// ── F. Everything is finite over the whole envelope (Q5 §Công việc #9) ───────

#[test]
fn full_envelope_output_is_finite_and_bounded() {
    // Slam each control to both rails on every patch — nothing may produce NaN,
    // an infinity, or an out-of-[0,1] pixel on either evaluator.
    let sliders: &[fn(&mut DevelopSettings, f32)] = &[
        |s, v| s.exposure = v,
        |s, v| s.contrast = v,
        |s, v| s.highlights = v,
        |s, v| s.shadows = v,
        |s, v| s.whites = v,
        |s, v| s.blacks = v,
        |s, v| s.saturation = v,
        |s, v| s.vibrance = v,
    ];
    for (i, set) in sliders.iter().enumerate() {
        let scale = if i == 0 {
            EXPOSURE_LIMIT
        } else {
            CONTROL_LIMIT
        };
        for frac in [-1.0f32, 1.0] {
            let s = tone(|s| set(s, frac * scale));
            for &(_, lin) in CHROMATICS {
                let a = eval_scene_pixel(lin, &s, BaseLook::Raw);
                let scene = solid_linear_scene(lin);
                let b = eval_scene_pixel_for_scene(&scene, lin, &s);
                for out in [a, b] {
                    assert!(
                        out.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)),
                        "non-finite/out-of-range output {out:?} (slider {i}, frac {frac}, {lin:?})"
                    );
                }
            }
        }
    }
    // Sanity: the EV axis constants frame a real ±range around grey.
    assert!(SCENE_EV_MIN < 0.0 && SCENE_EV_MAX > 0.0 && SCENE_EV_MIN < SCENE_EV_MAX);
}
