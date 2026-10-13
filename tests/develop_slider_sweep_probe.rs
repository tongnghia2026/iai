//! Quality Milestone Q0 — slider-behaviour baseline (hermetic golden/property).
//!
//! The RAM/quality plan (Q0 §3.3 and §8.4) requires a *visible baseline* of how
//! each main Develop slider behaves before any look is tuned: sweep each control
//! across its range with the others neutral, and record the tonal/colour
//! response so "wrong hue / clips early / not monotonic" becomes measurable.
//!
//! Unlike the corpus probe, this needs no RAW files: it feeds a fixed set of
//! synthetic linear-sRGB patches (a neutral staircase + saturated primaries +
//! skin/sky) through the deterministic scene evaluator
//! [`iai::core::develop_scene::eval_scene_pixel`] and measures the encoded-sRGB
//! output in OKLab. Because it is deterministic and fast, it runs in the normal
//! `cargo test` gate and doubles as a regression guard on slider behaviour.
//!
//! Sweep points are ±100% / ±50% / 0 of each slider's own UI range (exposure
//! ±`EXPOSURE_LIMIT`, the tone/colour controls ±`CONTROL_LIMIT`). Set
//! `IAI_Q0_OUT` to also write `q0_slider_sweep.csv` for side-by-side inspection.

use iai::core::develop::{DevelopSettings, CONTROL_LIMIT, EXPOSURE_LIMIT};
use iai::core::develop_scene::{eval_scene_pixel, BaseLook};
use iai::core::perceptual_color::linear_srgb_to_oklab;

/// Encoded-sRGB → linear. Local copy so the probe is self-contained.
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

struct Patch {
    name: &'static str,
    /// Linear-sRGB working value fed to the evaluator.
    linear: [f32; 3],
    neutral: bool,
}

/// Neutral staircase (shadow→highlight, 18% grey at index 3) plus saturated
/// primaries and two memory colours. Neutrals must stay neutral through every
/// non-white-balance slider; the chromatic patches expose hue rotation.
const PATCHES: &[Patch] = &[
    Patch {
        name: "gray_02",
        linear: [0.02, 0.02, 0.02],
        neutral: true,
    },
    Patch {
        name: "gray_06",
        linear: [0.06, 0.06, 0.06],
        neutral: true,
    },
    Patch {
        name: "gray_12",
        linear: [0.12, 0.12, 0.12],
        neutral: true,
    },
    Patch {
        name: "gray_18",
        linear: [0.184, 0.184, 0.184],
        neutral: true,
    },
    Patch {
        name: "gray_35",
        linear: [0.35, 0.35, 0.35],
        neutral: true,
    },
    Patch {
        name: "gray_60",
        linear: [0.60, 0.60, 0.60],
        neutral: true,
    },
    Patch {
        name: "gray_90",
        linear: [0.90, 0.90, 0.90],
        neutral: true,
    },
    Patch {
        name: "red",
        linear: [0.60, 0.05, 0.05],
        neutral: false,
    },
    Patch {
        name: "green",
        linear: [0.05, 0.50, 0.05],
        neutral: false,
    },
    Patch {
        name: "blue",
        linear: [0.04, 0.05, 0.50],
        neutral: false,
    },
    Patch {
        name: "cyan",
        linear: [0.05, 0.45, 0.50],
        neutral: false,
    },
    Patch {
        name: "magenta",
        linear: [0.50, 0.05, 0.45],
        neutral: false,
    },
    Patch {
        name: "yellow",
        linear: [0.60, 0.55, 0.05],
        neutral: false,
    },
    Patch {
        name: "skin",
        linear: [0.45, 0.28, 0.20],
        neutral: false,
    },
    Patch {
        name: "sky",
        linear: [0.12, 0.22, 0.50],
        neutral: false,
    },
];

type Setter = fn(&mut DevelopSettings, f32);

/// (label, setter, full-scale UI value, is_tone). `is_tone` marks the Light
/// controls, which must preserve hue on chromatic patches; Saturation/Vibrance
/// change chroma by design so their hue drift is recorded but not asserted.
const SLIDERS: &[(&str, Setter, f32, bool)] = &[
    ("exposure", |s, v| s.exposure = v, EXPOSURE_LIMIT, true),
    ("contrast", |s, v| s.contrast = v, CONTROL_LIMIT, true),
    ("highlights", |s, v| s.highlights = v, CONTROL_LIMIT, true),
    ("shadows", |s, v| s.shadows = v, CONTROL_LIMIT, true),
    ("whites", |s, v| s.whites = v, CONTROL_LIMIT, true),
    ("blacks", |s, v| s.blacks = v, CONTROL_LIMIT, true),
    ("saturation", |s, v| s.saturation = v, CONTROL_LIMIT, false),
    ("vibrance", |s, v| s.vibrance = v, CONTROL_LIMIT, false),
];

const FRACTIONS: [f32; 5] = [-1.0, -0.5, 0.0, 0.5, 1.0];

#[derive(Clone, Copy)]
struct Measured {
    l: f32,
    chroma: f32,
    hue_deg: f32,
    clipped: bool,
}

fn measure(linear: [f32; 3], settings: &DevelopSettings) -> Measured {
    let out = eval_scene_pixel(linear, settings, BaseLook::Raw);
    let lab = linear_srgb_to_oklab(out.map(srgb_to_linear));
    Measured {
        l: lab.l,
        chroma: lab.a.hypot(lab.b),
        hue_deg: lab.b.atan2(lab.a).to_degrees(),
        clipped: out.iter().any(|&c| c <= 0.001 || c >= 0.999),
    }
}

/// Smallest absolute angle (deg) between two hues on the colour circle.
fn hue_gap(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

#[test]
fn develop_slider_sweep_baseline() {
    // Per (slider, fraction, patch) measurement, plus the golden aggregates the
    // plan cares about: neutral chroma, exposure monotonicity, tone hue drift.
    let mut csv =
        String::from("slider,fraction,value,patch,neutral,out_L,out_chroma,out_hue_deg,clipped\n");
    let mut max_neutral_chroma = 0.0f32;
    let mut max_tone_hue_drift = 0.0f32;
    let mut worst_tone_hue: (&str, &str, f32) = ("", "", 0.0);
    let mut exposure_mid_l = [0.0f32; FRACTIONS.len()];

    for &(name, setter, scale, is_tone) in SLIDERS {
        // Baseline hue per chromatic patch at fraction 0, to measure drift.
        let mut base_hue = std::collections::HashMap::new();
        {
            let settings = DevelopSettings::default();
            for patch in PATCHES.iter().filter(|p| !p.neutral) {
                base_hue.insert(patch.name, measure(patch.linear, &settings).hue_deg);
            }
        }

        for (fraction_index, &fraction) in FRACTIONS.iter().enumerate() {
            let value = fraction * scale;
            let mut settings = DevelopSettings::default();
            setter(&mut settings, value);

            for patch in PATCHES {
                let m = measure(patch.linear, &settings);
                assert!(
                    m.l.is_finite() && m.chroma.is_finite() && m.hue_deg.is_finite(),
                    "{name}@{fraction}: {} produced non-finite output",
                    patch.name
                );
                if patch.neutral {
                    max_neutral_chroma = max_neutral_chroma.max(m.chroma);
                }
                if name == "exposure" && patch.name == "gray_18" {
                    exposure_mid_l[fraction_index] = m.l;
                }
                if is_tone && !patch.neutral {
                    let drift = hue_gap(m.hue_deg, base_hue[patch.name]);
                    if drift > max_tone_hue_drift {
                        max_tone_hue_drift = drift;
                        worst_tone_hue = (name, patch.name, drift);
                    }
                }
                use std::fmt::Write as _;
                writeln!(
                    csv,
                    "{name},{fraction:+.1},{value:+.1},{},{},{:.4},{:.4},{:.1},{}",
                    patch.name, patch.neutral, m.l, m.chroma, m.hue_deg, m.clipped
                )
                .unwrap();
            }
        }
    }

    // ── Print the baseline the owner reads ────────────────────────────────────
    println!("\nQ0 slider-sweep baseline (BaseLook::Raw, synthetic patches)\n");
    println!(
        "neutral-preservation: max chroma on any grey across all sliders = {max_neutral_chroma:.4}"
    );
    println!(
        "tone hue-drift: worst = {:.1} deg ({} on {})",
        worst_tone_hue.2, worst_tone_hue.0, worst_tone_hue.1
    );
    println!("exposure gray_18 OKLab L across -100..+100%: {exposure_mid_l:?}");
    for &(name, setter, scale, _) in SLIDERS {
        let l: Vec<String> = FRACTIONS
            .iter()
            .map(|&f| {
                let mut s = DevelopSettings::default();
                setter(&mut s, f * scale);
                format!("{:.3}", measure(PATCHES[3].linear, &s).l) // gray_18
            })
            .collect();
        let clips: usize = FRACTIONS
            .iter()
            .map(|&f| {
                let mut s = DevelopSettings::default();
                setter(&mut s, f * scale);
                PATCHES
                    .iter()
                    .filter(|p| measure(p.linear, &s).clipped)
                    .count()
            })
            .sum();
        println!(
            "  {name:<11} gray_18 L={} | total clipped patch-steps={clips}",
            l.join(" ")
        );
    }

    if let Some(out) = std::env::var_os("IAI_Q0_OUT") {
        let dir = std::path::PathBuf::from(out);
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(dir.join("q0_slider_sweep.csv"), &csv);
            println!("\nwrote {}", dir.join("q0_slider_sweep.csv").display());
        }
    }

    // ── Golden invariants (locked to the 2026-08-26 baseline) ─────────────────
    // Exposure must raise mid-grey lightness monotonically across the sweep.
    for pair in exposure_mid_l.windows(2) {
        assert!(
            pair[1] > pair[0],
            "exposure must increase mid-grey lightness monotonically: {exposure_mid_l:?}"
        );
    }
    // …and span most of the display range end to end, proving the ±5 EV maps.
    assert!(
        exposure_mid_l[0] < 0.10 && *exposure_mid_l.last().unwrap() > 0.95,
        "exposure sweep must span shadow→highlight: {exposure_mid_l:?}"
    );
    // A true neutral must stay neutral through every non-WB slider. Observed
    // 0.0000 (neutrals are symmetric by construction); the bound only guards
    // against a future per-channel tone/colour bug tinting greys.
    assert!(
        max_neutral_chroma < 0.002,
        "a grey patch gained chroma {max_neutral_chroma:.4} — a slider is tinting neutrals"
    );
    // Light controls preserve hue on saturated colour. Observed worst 10.3 deg
    // (exposure on sky); lock just above so a hue-rotation regression trips.
    assert!(
        max_tone_hue_drift < 13.0,
        "tone slider rotated hue {max_tone_hue_drift:.1} deg on {} (via {}) — exceeds the 13 deg baseline",
        worst_tone_hue.1, worst_tone_hue.0
    );
}
