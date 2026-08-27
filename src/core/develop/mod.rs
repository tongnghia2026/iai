//! Develop: non-destructive RAW/photo adjustments (Light, Colour, Detail,
//! Effects, Curves, and local masks).
//!
//! This module is the display-domain engine and the shared vocabulary; the
//! scene-referred Light path lives in [`super::develop_scene`]. Submodules:
//! `settings` (data model), `curves`, `math`, `tone`, `mixer`, `color`,
//! `spatial`, `detail`, and `pipeline` (the per-tile apply orchestrator).
//! Tuning constants live here at the root, shared by every stage.

mod color;
mod curves;
mod detail;
mod math;
mod mixer;
mod pipeline;
mod settings;
mod spatial;
mod tone;

pub(crate) use self::color::*;
pub(crate) use self::curves::*;
pub(crate) use self::detail::*;
pub(crate) use self::math::*;
pub(crate) use self::mixer::*;
pub(crate) use self::pipeline::*;
pub(crate) use self::settings::*;
pub(crate) use self::spatial::*;
pub(crate) use self::tone::*;

// Explicit public facade. The globs above stay crate-internal; only the
// entry points that integration tests (tests/perf_develop.rs) drive are
// exported, so the crate's public surface stays small.
pub use self::curves::build_histogram_proxy;
pub use self::mixer::{mixer_mask_preview, mixer_target_from_srgb, MixerTarget};
pub use self::settings::{
    ColorMixerAlgorithm, DevelopEngineVersion, DevelopSettings, PointCurveMode, ToneMapMode,
};
pub use self::spatial::{apply_color_to_region, fast_preview_downsample, guided_mixer_controls};

pub const MIXER_BANDS: usize = 8;
pub const MIXER_LABELS: [&str; MIXER_BANDS] = [
    "Reds", "Oranges", "Yellows", "Greens", "Aquas", "Blues", "Purples", "Magentas",
];
pub const MIXER_COLORS: [[u8; 3]; MIXER_BANDS] = [
    [210, 69, 69],
    [214, 128, 52],
    [202, 184, 62],
    [78, 174, 92],
    [70, 178, 178],
    [83, 120, 214],
    [120, 82, 198],
    [195, 76, 155],
];
pub const CONTROL_LIMIT: f32 = 200.0;
pub const EXPOSURE_LIMIT: f32 = 50.0;

/// Radius (px, full-res) of the edge-aware guided filter behind the colour
/// stage's regional low-pass. Wide so an off-hue speck inside a region (a red
/// blemish in orange skin) is dominated by its surroundings and inherits the
/// region's adjustment, while the guided filter keeps true edges (lips/eyes).
pub(crate) const COLOR_REGION_RADIUS: usize = 12;
/// The region-aware colour adjustment (guided filter + the heavy per-pixel
/// oklab/HSL math) is computed on a 1/N proxy and bilinear-upsampled. This makes
/// the live preview cheap (≈N² fewer transcendental calls) and, because the
/// upsample interpolates, the adjustment transitions across colour patches stay
/// soft instead of hard-edged.
pub(crate) const COLOR_DOWNSAMPLE: usize = 6;
const FAST_PREVIEW_MIN_DOWNSAMPLE: usize = 8;
const FAST_PREVIEW_MAX_DOWNSAMPLE: usize = 48;
/// Guided-filter regularisation. Window variance below this (~std 0.16 in [0,1])
/// is treated as flat and smoothed into the region; above it is an edge and
/// preserved. Tuned so skin tone unifies softly but skin↔lip/eye edges hold.
pub(crate) const COLOR_GUIDED_EPS: f32 = 0.025;
/// Fraction of the *chromatic* detail re-added after boosting the low-pass.
/// Luminance detail (texture) is always kept in full; keeping only part of the
/// chroma detail lets off-hue specks follow the region (lower = follow harder)
/// and softens residual block noise without smudging texture.
const CHROMA_DETAIL_KEEP: f32 = 0.5;
/// Max Oklab-hue rotation (degrees) the Color Mixer's Hue slider applies at full
/// strength on a pure-band pixel. Tunable "feel" knob vs PTS.
const MIXER_HUE_SHIFT_MAX_DEG: f32 = 45.0;
// Linear-light chroma needs a longer positive range than the former gamma-space
// transform to retain the same perceptual slider travel without shifting Y.
const SAT_POSITIVE_SCALE: f32 = 1.50;
const MIXER_SAT_POSITIVE_SCALE: f32 = 2.20;
const SAT_NEGATIVE_SCALE: f32 = 1.0;

// ── Colour-Mixer selection model (hue-curve colour equalizer) ───────────────
// A pixel is selected by its UCS-22 hue (core/ucs.rs) through a
// smooth PERIODIC curve interpolating the 8 band sliders — no band boundaries,
// no per-band profiles or special cases — then weighted by its HSV saturation
// through a steep logistic so greys and near-neutrals take none of the edit.
// (HSV saturation = delta/max is luma-normalised: navy, burgundy and dark
// foliage measure HIGH and stay fully selectable, unlike a chroma metric.)
//
// The 8 curve nodes sit at the UCS hue of the UI band swatches (MIXER_COLORS),
// so the colour the user sees on a slider is literally the hue that answers
// most. Node positions are fixed → the periodic-RBF interpolation collapses
// into one static Lagrange basis (`mixer_basis`, MIXER_CURVE_RES × 8) shared
// by the CPU curves and the GPU re-gate LUT — parity by construction.

/// Entries in each interpolated mixer curve (1° hue resolution, matching the
/// reference implementation's LUT).
pub(crate) const MIXER_CURVE_RES: usize = 360;
/// Periodic-RBF kernel K(d) = exp(Σ_l exp(−l²/c)·cos(l·d)), c = π, truncated
/// after m = ⌈3√c⌉ cosine terms (the next term is < 4e-4 — below f32 noise).
const MIXER_RBF_SMOOTHING: f32 = std::f32::consts::PI;
const MIXER_RBF_TERMS: usize = 6;
/// Logistic saturation weight: steepness and midpoints. Luminance edits demand
/// more saturation (higher midpoint): brightening a barely-coloured pixel
/// reads as tone damage, not colour grading — the same asymmetry the reference
/// module applies to its brightness corrections.
///
/// The steepness is deliberately GENTLER than the reference default (60): an
/// 8-bit JPEG quantizes dark pixels so coarsely that HSV saturation jumps in
/// steps of ~0.05–0.1 between neighbouring chroma blocks, and a steep logistic
/// flips the weight 0↔1 across them — hard square seams along hair/shadow
/// boundaries. 24 turns those flips into a smooth ramp; 16-bit RAW never
/// noticed either way.
const MIXER_SAT_STEEP: f32 = 24.0;
const MIXER_SAT_SHIFT: f32 = 0.06;
const MIXER_BRIGHT_SHIFT: f32 = 0.10;
/// Absolute-delta (max−min, display units) confidence gate multiplied into the
/// saturation weight. HSV saturation divides by the channel MAX, so a dark
/// near-neutral with a tiny absolute delta (e.g. rgb [32,33,35] → delta 0.012,
/// HSV sat 0.086) reads as "somewhat saturated" and would catch shadow noise;
/// requiring a real absolute separation rejects it while navy/burgundy/foliage
/// (delta ≥ 0.08) pass untouched. The window is WIDE (16 8-bit steps) for the
/// same JPEG reason as the steepness above — a narrow window flipped whole
/// chroma blocks on/off in dark regions.
const MIXER_DELTA_LO: f32 = 0.012;
const MIXER_DELTA_HI: f32 = 0.075;
// Luminance-edit shadow/highlight guard. The shadow knee is LOW so a dark-but-
// coloured pixel (navy/foliage/burgundy, luma ≈ 0.12–0.20) takes a real Luminance
// edit; only genuinely near-black (luma < ~0.08) is held, which still stops black
// speckles on darkening. Highlights stay guarded against wash-out.
const LUM_BLACK_LO: f32 = 0.02;
const LUM_BLACK_HI: f32 = 0.14;
const LUM_WHITE_LO: f32 = 0.82;
const LUM_WHITE_HI: f32 = 0.98;

// Full-resolution reconstruction (`finish_colored_pixel_f32`) spatial guard.
// The re-gate is a MEMBERSHIP indicator, not a second copy of the graded
// affinity: once a pixel belongs to an edited hue its edit passes at full
// strength (the graded strength is already baked into the `adjusted` proxy by
// `apply_color`). The knees sit BELOW the curve's response onset: the smooth
// periodic curve gives in-between hues (e.g. skin under a Red edit) a real
// partial response, so the gate must already be fully open there or the live
// proxy under-delivers vs the commit (proxyΔ ≠ directΔ). Only true neutrals
// and far hues (membership ≈ curve ripple) stay closed — matching their
// near-zero direct response.
const REGATE_LO: f32 = 0.02;
const REGATE_HI: f32 = 0.12;
/// Region-luma gradient (per proxy texel, central difference) where the colour
/// correction starts / is fully faded out. A strong object edge measures a
/// gradient near the luma step; gentle shading measures well below `LO`, so an
/// object's interior keeps its full edit while the edit tapers across a real
/// boundary (colour-equalizer halo/gradient suppression).
const EDGE_SUPPRESS_LO: f32 = 0.06;
const EDGE_SUPPRESS_HI: f32 = 0.30;
/// Same suppressor, but on the gradient of the EDIT's LUMA (Δbrightness =
/// luma(adjusted) − luma(region)) rather than raw region luma. This targets the
/// one edit that makes a *visible* halo — a Luminance Red/Orange boost lifts warm
/// skin ~0.1–0.2 above the neutral background, a real brightness boundary that
/// must fade even though the pixels shared the same luma before the edit (so the
/// region-luma term above measures ~0 there). Keying on the correction's luma (not
/// its full RGB vector) leaves Saturation/Hue edits — which barely move luma and
/// whose colour boundaries are already tapered by the per-pixel band affinity —
/// essentially unshaped. Knees are low because a Δbrightness step is small; a
/// shallow one (gentle grade) stays near full.
const EDIT_SUPPRESS_LO: f32 = 0.02;
const EDIT_SUPPRESS_HI: f32 = 0.10;
/// Floor on the edge-suppression weight: the boost is softened toward a boundary
/// but never dragged below this fraction, so the fade does not eat a dark rim
/// deep INTO the colour that is being brightened (skin near the white-grey
/// background / near dark hair keeps most of its lift). Safe to floor because the
/// neutral background is held out by the mixer's own near-zero correction and
/// genuinely dark pixels by the per-pixel shadow gate in `finish_colored_pixel_f32`
/// — both independent of this suppressor — so lifting the floor cannot re-halo the
/// grey/dark side, it only returns edit to the colour side. Lower = softer edge
/// but more encroachment into the colour; higher = crisper colour up to the edge.
const EDGE_FADE_FLOOR: f32 = 0.4;
/// Chroma lift for the shadow fade so a dark *coloured* pixel (navy, burgundy,
/// dark foliage) keeps its edit, while a dark *near-neutral* rim still fades out.
/// Luma alone must not gate colour — this is the same chroma-rescue idea the
/// neutral protection uses.
const SHADOW_COLOR_RESCUE: f32 = 0.60;
/// Radius (px) of the regional luminance behind the Shadows/Highlights local
/// adaptation. Wider than the colour radius: shadow lift should follow broad
/// lighting regions (not local texture) so detail/contrast is preserved.
pub(crate) const TONE_REGION_RADIUS: usize = 24;
/// Guided-filter epsilon for the Shadows/Highlights/Blacks regional base luminance.
/// Higher than the colour stage's `COLOR_GUIDED_EPS` on purpose: the regional luma
/// should be a SMOOTH ambient field so the lift is uniform within a region (the
/// pixel's own texture/local-contrast is preserved separately, via the global LUT on
/// pixel luma). A lower eps preserved mid-frequency structure (e.g. foliage), so a
/// steep COMBINED lift (Contrast+Blacks+Shadows) made that structure surface as
/// blotchy "loang" with hard-ish region boundaries. Not so high that big light/dark
/// edges halo.
const TONE_GUIDED_EPS: f32 = 0.05;
/// The Shadows/Highlights regional base luminance is a smooth (24px-blurred)
/// signal, so the edge-aware guided filter behind it is computed on a 1/N proxy
/// and bilinear-upsampled — same trick the colour stage uses. Cuts the guided
/// filter's box-blur passes by ~N², which is the dominant per-tile cost when
/// Shadows/Highlights/Whites/Blacks drag. Smaller than `COLOR_DOWNSAMPLE` to keep
/// a bit more edge fidelity (a bright strand inside a dark region must survive).
pub const TONE_DOWNSAMPLE: usize = 4;

#[cfg(test)]
mod tests;
