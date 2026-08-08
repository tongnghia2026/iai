//! Develop data model: the `DevelopSettings` snapshot plus its local-adjustment
//! and mask types, and the neutral / diff / capability queries the app and the
//! apply pipeline use to decide what work a settings change requires.

use super::*;

/// Which channel the Colour Mixer panel is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DevelopMixerMode {
    Hue,
    Saturation,
    Luminance,
    All,
}

/// Which local-mask tool the panel arms for canvas placement (view state,
/// not serialized).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalMaskKind {
    Linear,
    Radial,
}

/// Geometry of one local-adjustment mask, in image-normalized coordinates
/// (x/(w−1), y/(h−1)) so a saved preset fits any image size.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LocalMaskShape {
    /// Full effect on the `(x0,y0)` side, fading to zero along the line
    /// towards `(x1,y1)` (LR-style linear gradient).
    Linear { x0: f32, y0: f32, x1: f32, y1: f32 },
    /// Axis-aligned ellipse: full effect inside, fading across the feather
    /// band at the rim; `invert` applies the effect outside instead.
    Radial {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        /// 0 = hard rim, 1 = fade from the centre.
        feather: f32,
        invert: bool,
    },
}

impl LocalMaskShape {
    pub fn kind(&self) -> LocalMaskKind {
        match self {
            LocalMaskShape::Linear { .. } => LocalMaskKind::Linear,
            LocalMaskShape::Radial { .. } => LocalMaskKind::Radial,
        }
    }

    /// Shape from a canvas drag (normalized start → current), used both for
    /// live placement and the final geometry on release.
    pub fn from_drag(kind: LocalMaskKind, x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        match kind {
            LocalMaskKind::Linear => LocalMaskShape::Linear { x0, y0, x1, y1 },
            LocalMaskKind::Radial => LocalMaskShape::Radial {
                cx: x0,
                cy: y0,
                rx: (x1 - x0).abs().max(0.01),
                ry: (y1 - y0).abs().max(0.01),
                feather: 0.5,
                invert: false,
            },
        }
    }

    /// Mask weight at normalized image coords, in [0,1].
    pub fn weight(&self, nx: f32, ny: f32) -> f32 {
        match *self {
            LocalMaskShape::Linear { x0, y0, x1, y1 } => {
                let dx = x1 - x0;
                let dy = y1 - y0;
                let len2 = (dx * dx + dy * dy).max(1e-8);
                let t = ((nx - x0) * dx + (ny - y0) * dy) / len2;
                1.0 - smootherstep(0.0, 1.0, t)
            }
            LocalMaskShape::Radial {
                cx,
                cy,
                rx,
                ry,
                feather,
                invert,
            } => {
                let dx = (nx - cx) / rx.max(1e-4);
                let dy = (ny - cy) / ry.max(1e-4);
                let rho = (dx * dx + dy * dy).sqrt();
                // Fade band runs from (1−feather)·rim to the rim; the epsilon
                // keeps feather=0 a well-defined (near-hard) edge.
                let inner = (1.0 - feather.clamp(0.0, 1.0)).min(1.0 - 1e-3);
                let w = 1.0 - smootherstep(inner, 1.0, rho);
                if invert {
                    1.0 - w
                } else {
                    w
                }
            }
        }
    }
}

/// The sliders one local mask can carry — the per-pixel subset of the Develop
/// stages (no spatial/detail terms, so a mask stays a pure point-op given its
/// weight).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LocalSettings {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
}

impl Default for LocalSettings {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            temperature: 0.0,
            tint: 0.0,
            saturation: 0.0,
        }
    }
}

impl LocalSettings {
    pub fn is_neutral(&self) -> bool {
        self.exposure.abs() <= 0.001
            && self.contrast.abs() <= 0.001
            && self.highlights.abs() <= 0.001
            && self.shadows.abs() <= 0.001
            && self.temperature.abs() <= 0.001
            && self.tint.abs() <= 0.001
            && self.saturation.abs() <= 0.001
    }

    /// Synthetic global settings carrying only this mask's sliders, so the
    /// local stage reuses the exact tone/colour builders of the global path.
    pub(crate) fn to_develop_settings(&self) -> DevelopSettings {
        DevelopSettings {
            exposure: self.exposure,
            contrast: self.contrast,
            highlights: self.highlights,
            shadows: self.shadows,
            temperature: self.temperature,
            tint: self.tint,
            saturation: self.saturation,
            ..Default::default()
        }
    }
}

/// One local adjustment: a mask and the sliders it carries.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LocalAdjustment {
    pub shape: LocalMaskShape,
    pub settings: LocalSettings,
}

/// Serialized into .iai for non-destructive Develop records; `serde(default)`
/// keeps old files loadable as sliders are added.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DevelopSettings {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub temperature: f32,
    pub tint: f32,
    pub vibrance: f32,
    pub saturation: f32,
    /// Split-grade hue in degrees. Hue is inert while its strength is zero.
    pub grade_shadow_hue: f32,
    pub grade_shadow_strength: f32,
    pub grade_highlight_hue: f32,
    pub grade_highlight_strength: f32,
    pub texture: f32,
    pub clarity: f32,
    pub sharpening: f32,
    /// Unsharp-mask radius in pixels (0.5–3.0). Modifier of `sharpening` — has
    /// no effect while the amount is 0, so it is excluded from `is_neutral`.
    pub sharpen_radius: f32,
    /// 0–100: how much small-amplitude (fine texture) high-pass passes through.
    /// Low values sharpen only real edges, protecting noise and skin.
    pub sharpen_detail: f32,
    /// 0–100: edge mask threshold — higher protects smooth areas entirely.
    pub sharpen_masking: f32,
    pub noise_reduction: f32,
    pub color_noise_reduction: f32,
    pub dehaze: f32,
    pub vignette: f32,
    pub curve_highlights: f32,
    pub curve_lights: f32,
    pub curve_darks: f32,
    pub curve_shadows: f32,
    /// Point curve on the luminance axis: sorted (x, y) control points in
    /// [0,1]², interpolated with a monotone Hermite spline and applied ON TOP
    /// of the parametric curve. `[[0,0],[1,1]]` (or any points on the
    /// diagonal) = identity.
    pub curve_points: Vec<[f32; 2]>,
    /// Per-channel point curves, applied to R/G/B after the tone stage.
    pub curve_points_r: Vec<[f32; 2]>,
    pub curve_points_g: Vec<[f32; 2]>,
    pub curve_points_b: Vec<[f32; 2]>,
    pub mixer_mode: DevelopMixerMode,
    pub mixer_hue: [f32; MIXER_BANDS],
    pub mixer_saturation: [f32; MIXER_BANDS],
    pub mixer_luminance: [f32; MIXER_BANDS],
    /// Local adjustments (gradient/radial masks with their own sliders),
    /// applied after the global stages. `serde(default)` keeps older files
    /// and presets loadable.
    pub locals: Vec<LocalAdjustment>,
}

impl Default for DevelopSettings {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            temperature: 0.0,
            tint: 0.0,
            vibrance: 0.0,
            saturation: 0.0,
            grade_shadow_hue: 220.0,
            grade_shadow_strength: 0.0,
            grade_highlight_hue: 35.0,
            grade_highlight_strength: 0.0,
            texture: 0.0,
            clarity: 0.0,
            sharpening: 0.0,
            sharpen_radius: 1.0,
            sharpen_detail: 25.0,
            sharpen_masking: 0.0,
            noise_reduction: 0.0,
            color_noise_reduction: 0.0,
            dehaze: 0.0,
            vignette: 0.0,
            curve_highlights: 0.0,
            curve_lights: 0.0,
            curve_darks: 0.0,
            curve_shadows: 0.0,
            curve_points: identity_curve(),
            curve_points_r: identity_curve(),
            curve_points_g: identity_curve(),
            curve_points_b: identity_curve(),
            mixer_mode: DevelopMixerMode::Saturation,
            mixer_hue: [0.0; MIXER_BANDS],
            mixer_saturation: [0.0; MIXER_BANDS],
            mixer_luminance: [0.0; MIXER_BANDS],
            locals: Vec::new(),
        }
    }
}

#[inline]
fn same_grade(hue: f32, strength: f32, other_hue: f32, other_strength: f32) -> bool {
    strength == other_strength && (strength.abs() <= 0.001 || hue == other_hue)
}

impl DevelopSettings {
    pub fn is_neutral(&self) -> bool {
        self.exposure.abs() <= 0.001
            && self.contrast.abs() <= 0.001
            && self.highlights.abs() <= 0.001
            && self.shadows.abs() <= 0.001
            && self.whites.abs() <= 0.001
            && self.blacks.abs() <= 0.001
            && self.temperature.abs() <= 0.001
            && self.tint.abs() <= 0.001
            && self.vibrance.abs() <= 0.001
            && self.saturation.abs() <= 0.001
            && self.grade_shadow_strength.abs() <= 0.001
            && self.grade_highlight_strength.abs() <= 0.001
            && self.texture.abs() <= 0.001
            && self.clarity.abs() <= 0.001
            && self.sharpening.abs() <= 0.001
            && self.noise_reduction.abs() <= 0.001
            && self.color_noise_reduction.abs() <= 0.001
            && self.dehaze.abs() <= 0.001
            && self.vignette.abs() <= 0.001
            && self.curve_highlights.abs() <= 0.001
            && self.curve_lights.abs() <= 0.001
            && self.curve_darks.abs() <= 0.001
            && self.curve_shadows.abs() <= 0.001
            && curve_is_identity(&self.curve_points)
            && curve_is_identity(&self.curve_points_r)
            && curve_is_identity(&self.curve_points_g)
            && curve_is_identity(&self.curve_points_b)
            && self.mixer_hue.iter().all(|v| v.abs() <= 0.001)
            && self.mixer_saturation.iter().all(|v| v.abs() <= 0.001)
            && self.mixer_luminance.iter().all(|v| v.abs() <= 0.001)
            && self.locals.iter().all(|l| l.settings.is_neutral())
    }

    pub fn same_image_effect(&self, other: &Self) -> bool {
        self.exposure == other.exposure
            && self.contrast == other.contrast
            && self.highlights == other.highlights
            && self.shadows == other.shadows
            && self.whites == other.whites
            && self.blacks == other.blacks
            && self.temperature == other.temperature
            && self.tint == other.tint
            && self.vibrance == other.vibrance
            && self.saturation == other.saturation
            && same_grade(
                self.grade_shadow_hue,
                self.grade_shadow_strength,
                other.grade_shadow_hue,
                other.grade_shadow_strength,
            )
            && same_grade(
                self.grade_highlight_hue,
                self.grade_highlight_strength,
                other.grade_highlight_hue,
                other.grade_highlight_strength,
            )
            && self.texture == other.texture
            && self.clarity == other.clarity
            && self.sharpening == other.sharpening
            && self.sharpen_radius == other.sharpen_radius
            && self.sharpen_detail == other.sharpen_detail
            && self.sharpen_masking == other.sharpen_masking
            && self.noise_reduction == other.noise_reduction
            && self.color_noise_reduction == other.color_noise_reduction
            && self.dehaze == other.dehaze
            && self.vignette == other.vignette
            && self.curve_highlights == other.curve_highlights
            && self.curve_lights == other.curve_lights
            && self.curve_darks == other.curve_darks
            && self.curve_shadows == other.curve_shadows
            && self.curve_points == other.curve_points
            && self.curve_points_r == other.curve_points_r
            && self.curve_points_g == other.curve_points_g
            && self.curve_points_b == other.curve_points_b
            && self.mixer_hue == other.mixer_hue
            && self.mixer_saturation == other.mixer_saturation
            && self.mixer_luminance == other.mixer_luminance
            && self.locals == other.locals
    }

    pub fn differs_only_color_mixer(&self, other: &Self) -> bool {
        self.exposure == other.exposure
            && self.contrast == other.contrast
            && self.highlights == other.highlights
            && self.shadows == other.shadows
            && self.whites == other.whites
            && self.blacks == other.blacks
            && self.temperature == other.temperature
            && self.tint == other.tint
            && self.vibrance == other.vibrance
            && self.saturation == other.saturation
            && same_grade(
                self.grade_shadow_hue,
                self.grade_shadow_strength,
                other.grade_shadow_hue,
                other.grade_shadow_strength,
            )
            && same_grade(
                self.grade_highlight_hue,
                self.grade_highlight_strength,
                other.grade_highlight_hue,
                other.grade_highlight_strength,
            )
            && self.texture == other.texture
            && self.clarity == other.clarity
            && self.sharpening == other.sharpening
            && self.sharpen_radius == other.sharpen_radius
            && self.sharpen_detail == other.sharpen_detail
            && self.sharpen_masking == other.sharpen_masking
            && self.noise_reduction == other.noise_reduction
            && self.color_noise_reduction == other.color_noise_reduction
            && self.dehaze == other.dehaze
            && self.vignette == other.vignette
            && self.curve_highlights == other.curve_highlights
            && self.curve_lights == other.curve_lights
            && self.curve_darks == other.curve_darks
            && self.curve_shadows == other.curve_shadows
            && self.curve_points == other.curve_points
            && self.curve_points_r == other.curve_points_r
            && self.curve_points_g == other.curve_points_g
            && self.curve_points_b == other.curve_points_b
            && self.locals == other.locals
            && (self.mixer_hue != other.mixer_hue
                || self.mixer_saturation != other.mixer_saturation
                || self.mixer_luminance != other.mixer_luminance)
    }

    /// True when `other` differs from `self` in ONLY the white-balance controls
    /// (Temperature and/or Tint) — every other field is identical.
    ///
    /// White balance is applied per-pixel by the shader (the CAT16·2^EV matrix),
    /// and the region proxies a WB change touches are cheap downsampled refreshes
    /// — the same cost profile as a colour-mixer-only tweak. So, like that case,
    /// such an edit can recompose immediately every frame instead of being
    /// throttled; that is what keeps a Temperature/Tint drag smooth instead of
    /// stepping at the throttle rate when Colour/local-tone/Effects are engaged.
    pub fn differs_only_white_balance(&self, other: &Self) -> bool {
        self.exposure == other.exposure
            && self.contrast == other.contrast
            && self.highlights == other.highlights
            && self.shadows == other.shadows
            && self.whites == other.whites
            && self.blacks == other.blacks
            && self.vibrance == other.vibrance
            && self.saturation == other.saturation
            && same_grade(
                self.grade_shadow_hue,
                self.grade_shadow_strength,
                other.grade_shadow_hue,
                other.grade_shadow_strength,
            )
            && same_grade(
                self.grade_highlight_hue,
                self.grade_highlight_strength,
                other.grade_highlight_hue,
                other.grade_highlight_strength,
            )
            && self.texture == other.texture
            && self.clarity == other.clarity
            && self.sharpening == other.sharpening
            && self.sharpen_radius == other.sharpen_radius
            && self.sharpen_detail == other.sharpen_detail
            && self.sharpen_masking == other.sharpen_masking
            && self.noise_reduction == other.noise_reduction
            && self.color_noise_reduction == other.color_noise_reduction
            && self.dehaze == other.dehaze
            && self.vignette == other.vignette
            && self.curve_highlights == other.curve_highlights
            && self.curve_lights == other.curve_lights
            && self.curve_darks == other.curve_darks
            && self.curve_shadows == other.curve_shadows
            && self.curve_points == other.curve_points
            && self.curve_points_r == other.curve_points_r
            && self.curve_points_g == other.curve_points_g
            && self.curve_points_b == other.curve_points_b
            && self.mixer_hue == other.mixer_hue
            && self.mixer_saturation == other.mixer_saturation
            && self.mixer_luminance == other.mixer_luminance
            && self.locals == other.locals
            && (self.temperature != other.temperature || self.tint != other.tint)
    }

    /// True when Highlights/Shadows or Whites/Blacks are engaged. These get
    /// edge-aware local adaptation (region-based, detail-preserving); the GPU
    /// preview samples a regional base-luma proxy to reproduce it.
    pub fn has_local_tone(&self) -> bool {
        self.highlights.abs() > 0.001
            || self.shadows.abs() > 0.001
            || self.whites.abs() > 0.001
            || self.blacks.abs() > 0.001
    }

    /// True when Vibrance/Saturation or any Color Mixer band is engaged. These run
    /// the region-aware colour stage (guided-filter de-block + per-pixel oklab/HSL),
    /// which the GPU previews by sampling a `region`/`adjusted` proxy pair.
    pub fn has_color(&self) -> bool {
        has_color(self)
    }

    /// True when a Detail slider (Sharpening / Noise Reduction) is engaged. The
    /// live shader cannot run these (full-res neighbourhood passes), so the GPU
    /// preview schedules a debounced commit-quality CPU bake instead.
    pub fn has_detail(&self) -> bool {
        has_detail(self)
    }

    /// True when any local-adjustment mask carries non-neutral sliders. The
    /// live preview then takes the CPU path (the exact commit bake), since the
    /// shader does not evaluate masks.
    pub fn has_locals(&self) -> bool {
        self.locals.iter().any(|l| !l.settings.is_neutral())
    }

    /// True when a live preview needs NO region proxy: a global-tone-only edit (no
    /// Colour, no local tone, no Effects — Detail is not previewed). Such a preview is
    /// a pure per-pixel GPU recompose, so it can update every frame (immediate)
    /// instead of being throttled like an expensive proxy rebuild. This is what keeps
    /// Contrast/Exposure dragging smooth instead of stepping at the throttle rate.
    pub fn preview_proxy_free(&self) -> bool {
        !self.has_color() && !self.has_local_tone() && !has_effects(self) && !self.has_locals()
    }

    /// True when an Effects slider with a spatial component is engaged —
    /// Texture/Clarity/Defog compare each pixel against a regional base.
    pub fn has_spatial_effects(&self) -> bool {
        self.texture.abs() > 0.001 || self.clarity.abs() > 0.001 || self.dehaze.abs() > 0.001
    }
}
