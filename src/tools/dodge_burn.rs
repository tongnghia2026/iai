//! Dodge & Burn — paint to locally lighten (Dodge) or darken (Burn), targeting a
//! tonal range (Shadows / Midtones / Highlights) like the darkroom techniques they
//! are named after. Both share one struct + the `scrub` dab engine; the `burn`
//! flag flips the identity (two toolbar tools, one code path — same approach as
//! Repair wrapping Clone).

use super::scrub;
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneRange {
    Shadows,
    Midtones,
    Highlights,
}

impl ToneRange {
    pub fn label(&self) -> &'static str {
        match self {
            ToneRange::Shadows => "Shadows",
            ToneRange::Midtones => "Midtones",
            ToneRange::Highlights => "Highlights",
        }
    }
    pub fn to_u8(self) -> u8 {
        match self {
            ToneRange::Shadows => 0,
            ToneRange::Midtones => 1,
            ToneRange::Highlights => 2,
        }
    }
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => ToneRange::Shadows,
            2 => ToneRange::Highlights,
            _ => ToneRange::Midtones,
        }
    }
}

/// Luma → tonal-range weight (0..1). Peaks where the named range lives so the
/// effect concentrates on those tones (the standard Range menu).
#[inline]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(0.0001)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn tone_weight(l: f32, range: ToneRange) -> f32 {
    match range {
        ToneRange::Shadows => 1.0 - smoothstep(0.18, 0.62, l),
        ToneRange::Highlights => smoothstep(0.38, 0.82, l),
        ToneRange::Midtones => smoothstep(0.06, 0.36, l) * (1.0 - smoothstep(0.64, 0.94, l)),
    }
}

#[inline]
fn exposure_to_dab_strength(exposure: f32, spacing: f32) -> f32 {
    let e = exposure.clamp(0.0, 1.0);
    let spacing = spacing.clamp(0.03, 0.5);
    e.powf(1.35) * spacing * 0.85
}

/// Apply one dab's worth of dodge/burn to a pixel. `amount` already folds in
/// exposure × dab coverage. With `protect_tones` (standard raster editors default) the change is
/// multiplicative (multiply for burn / screen for dodge) which keeps hue stable and
/// resists clipping; without it the push is a flat additive offset on every channel.
#[inline]
fn apply_dodge_burn(dst: &mut [u8; 4], burn: bool, amount: f32, range: ToneRange, protect: bool) {
    if dst[3] == 0 {
        return;
    }
    let mut c = [
        dst[0] as f32 / 255.0,
        dst[1] as f32 / 255.0,
        dst[2] as f32 / 255.0,
    ];
    let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let a = (amount * tone_weight(l, range)).clamp(0.0, 1.0);
    if a <= 0.0001 {
        return;
    }
    for v in c.iter_mut() {
        let nv = if protect {
            if burn {
                *v * (1.0 - a)
            } else {
                1.0 - (1.0 - *v) * (1.0 - a)
            }
        } else if burn {
            *v - a
        } else {
            *v + a
        };
        *v = nv.clamp(0.0, 1.0);
    }
    dst[0] = (c[0] * 255.0).round() as u8;
    dst[1] = (c[1] * 255.0).round() as u8;
    dst[2] = (c[2] * 255.0).round() as u8;
}

pub struct DodgeBurnTool {
    pub burn: bool,
    pub size: f32,
    pub hardness: f32,
    /// Per-dab strength (standard raster editors "Exposure"), 0..1. Strokes build up as dabs
    /// overlap (airbrush-like).
    pub exposure: f32,
    pub range: ToneRange,
    pub protect_tones: bool,
    pub spacing: f32,
    last: (f32, f32),
}

impl DodgeBurnTool {
    fn new(burn: bool) -> Self {
        Self {
            burn,
            size: 60.0,
            hardness: 0.0,
            exposure: 0.5,
            range: ToneRange::Midtones,
            protect_tones: true,
            spacing: 0.1,
            last: (0.0, 0.0),
        }
    }
    pub fn dodge() -> Self {
        Self::new(false)
    }
    pub fn burn() -> Self {
        Self::new(true)
    }

    fn dab(&self, ctx: &mut ToolCtx, cx: f32, cy: f32) {
        let burn = self.burn;
        let exposure = exposure_to_dab_strength(self.exposure, self.spacing);
        let range = self.range;
        let protect = self.protect_tones;
        scrub::for_each_dab_pixel(
            ctx.canvas_mut(),
            cx,
            cy,
            self.size * 0.5,
            self.hardness,
            1.0,
            |_px, _py, cov, dst| apply_dodge_burn(dst, burn, exposure * cov, range, protect),
        );
    }
}

impl Tool for DodgeBurnTool {
    fn id(&self) -> &'static str {
        if self.burn {
            "burn"
        } else {
            "dodge"
        }
    }
    fn name(&self) -> &str {
        if self.burn {
            "Burn"
        } else {
            "Dodge"
        }
    }
    fn shortcut(&self) -> Option<char> {
        Some('O')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        if self.burn {
            crate::tools::ToolId::Burn
        } else {
            crate::tools::ToolId::Dodge
        }
    }
    fn paints(&self) -> bool {
        true
    }
    fn cursor_size(&self) -> f32 {
        self.size * 0.5
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        ctx.canvas_mut()
            .begin_stroke(if self.burn { "Burn" } else { "Dodge" });
        self.last = (event.canvas_x, event.canvas_y);
        self.dab(ctx, event.canvas_x, event.canvas_y);
        ToolResponse::repaint()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        let (x0, y0) = self.last;
        let (x1, y1) = (event.canvas_x, event.canvas_y);
        let diameter = self.size;
        let spacing = self.spacing;
        // Collect step centres first so the dab closure doesn't borrow self twice.
        let mut pts: Vec<(f32, f32)> = Vec::new();
        scrub::dab_segment(x0, y0, x1, y1, diameter, spacing, |x, y| pts.push((x, y)));
        for (x, y) in pts {
            self.dab(ctx, x, y);
        }
        self.last = (x1, y1);
        ToolResponse::repaint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dodge_lightens_burn_darkens_midtone() {
        let mut d = [128u8, 128, 128, 255];
        apply_dodge_burn(&mut d, false, 0.5, ToneRange::Midtones, true);
        assert!(d[0] > 128, "dodge should lighten");

        let mut b = [128u8, 128, 128, 255];
        apply_dodge_burn(&mut b, true, 0.5, ToneRange::Midtones, true);
        assert!(b[0] < 128, "burn should darken");
    }

    #[test]
    fn transparent_pixel_untouched() {
        let mut t = [10u8, 20, 30, 0];
        apply_dodge_burn(&mut t, false, 1.0, ToneRange::Midtones, true);
        assert_eq!(t, [10, 20, 30, 0]);
    }

    #[test]
    fn range_targets_tones() {
        // A near-black pixel is barely affected when targeting Highlights.
        let mut dark = [10u8, 10, 10, 255];
        apply_dodge_burn(&mut dark, false, 0.8, ToneRange::Highlights, true);
        assert!(dark[0] <= 12, "highlights range should spare shadows");
    }

    #[test]
    fn exposure_slider_maps_to_gentle_per_dab_strength() {
        let mid = exposure_to_dab_strength(0.5, 0.1);
        assert!(mid > 0.02 && mid < 0.05, "50% should be gentle, got {mid}");

        let full = exposure_to_dab_strength(1.0, 0.1);
        assert!(
            full > mid && full < 0.1,
            "100% should still be per-dab bounded, got {full}"
        );
    }

    #[test]
    fn tone_ranges_feather_into_neighbors() {
        let shadow_mid = tone_weight(0.5, ToneRange::Shadows);
        let highlight_mid = tone_weight(0.5, ToneRange::Highlights);
        assert!(
            shadow_mid > 0.0 && shadow_mid < 0.35,
            "shadows should fade softly through midtones: {shadow_mid}"
        );
        assert!(
            highlight_mid > 0.0 && highlight_mid < 0.35,
            "highlights should fade softly through midtones: {highlight_mid}"
        );
        assert!(tone_weight(0.5, ToneRange::Midtones) > 0.95);
    }
}
