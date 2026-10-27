//! Measuring what a page shows, so an agent checks its work with numbers.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{RectPx, color_text};
use crate::{
    CmdError, Command, CommandMeta, Effect, ErrorCode, Plan, Session, Validate, layer_index,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RegionParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The part to measure, in document pixels; the whole page when omitted.
    pub rect: Option<RectPx>,
    /// Measure one layer's own pixels instead of what the page shows.
    pub layer: Option<u32>,
}
impl Validate for RegionParams {}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegionStats {
    pub doc: u32,
    /// The rectangle measured (the asked one, cut to the page).
    pub rect: RectPx,
    /// Average colour of the visible pixels, "#RRGGBB".
    pub mean: String,
    /// Spread of the colours around the mean, 0 (one flat colour) to ~128.
    pub std_dev: f32,
    /// Share of pixels not fully clear, 0 to 1.
    pub alpha_coverage: f32,
    /// Average lightness, 0 (black) to 1 (white).
    pub luminance: f32,
    /// Darkest and lightest lightness, 0 to 1.
    pub luminance_min: f32,
    pub luminance_max: f32,
}

static REGION: CommandMeta = CommandMeta {
    id: "analyze.region",
    label_vi: "Đo vùng ảnh",
    label_en: "Analyze Region",
    doc: "Measure a rectangle of what the page shows (or of one layer): average colour, colour \
          spread, how much is not clear, lightness. E.g. check an ID photo's backdrop is \
          #0090FF with a spread under 2.",
    effects: &[Effect::Query],
    featured: true,
};

pub struct AnalyzeRegion;

impl Command for AnalyzeRegion {
    type Params = RegionParams;
    type Output = RegionStats;
    fn meta(&self) -> &'static CommandMeta {
        &REGION
    }
    fn plan(&self, s: &mut Session<'_>, p: RegionParams) -> Result<Plan<RegionStats>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas(Some(doc))?;
        let (cw, ch) = (canvas.width, canvas.height);
        let rect = p.rect.unwrap_or(RectPx {
            x: 0,
            y: 0,
            w: cw.max(1),
            h: ch.max(1),
        });
        let Some((x0, y0, x1, y1)) = rect.clipped(cw, ch) else {
            return Err(
                CmdError::new(ErrorCode::BadParams, "the rectangle lies outside the page")
                    .vi("Vùng đo nằm ngoài ảnh"),
            );
        };
        let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        let rgba = match p.layer {
            None => canvas
                .layer_stack
                .flatten_compact_region(cw, ch, x0, y0, w, h),
            Some(id) => {
                let idx = layer_index(canvas, Some(id))?;
                let layer = canvas
                    .layer_stack
                    .layers
                    .get(idx)
                    .ok_or_else(|| CmdError::internal("layer index"))?;
                let mut out = vec![0u8; (w as usize).saturating_mul(h as usize).saturating_mul(4)];
                let (ox, oy) = layer.offset;
                for (row, line) in out
                    .chunks_mut((w as usize).saturating_mul(4).max(1))
                    .enumerate()
                {
                    let cy = i64::from(y0).saturating_add(row as i64);
                    for (col, px) in line.chunks_mut(4).enumerate() {
                        let cx = i64::from(x0).saturating_add(col as i64);
                        let (lx, ly) = (
                            cx.saturating_sub(i64::from(ox)),
                            cy.saturating_sub(i64::from(oy)),
                        );
                        if lx < 0
                            || ly < 0
                            || lx >= i64::from(layer.width)
                            || ly >= i64::from(layer.height)
                        {
                            continue;
                        }
                        let (r, g, b, a) = layer.tiles.get_pixel(lx as u32, ly as u32);
                        px.copy_from_slice(&[r, g, b, a]);
                    }
                }
                out
            }
        };
        let stats = measure(&rgba);
        Ok(Plan::Done(RegionStats {
            doc,
            rect: RectPx {
                x: x0 as i32,
                y: y0 as i32,
                w,
                h,
            },
            mean: color_text([stats.mean[0], stats.mean[1], stats.mean[2], 255]),
            std_dev: stats.std_dev,
            alpha_coverage: stats.coverage,
            luminance: stats.luminance,
            luminance_min: stats.lum_min,
            luminance_max: stats.lum_max,
        }))
    }
}

struct Stats {
    mean: [u8; 3],
    std_dev: f32,
    coverage: f32,
    luminance: f32,
    lum_min: f32,
    lum_max: f32,
}

/// Statistics of straight-alpha RGBA pixels; clear ones only count for
/// coverage.
fn measure(rgba: &[u8]) -> Stats {
    let mut n = 0u64;
    let mut seen = 0u64;
    let mut sum = [0f64; 3];
    let mut sq = [0f64; 3];
    let (mut lum_sum, mut lum_min, mut lum_max) = (0f64, 1f64, 0f64);
    for px in rgba.chunks_exact(4) {
        seen = seen.saturating_add(1);
        let &[r, g, b, a] = px else {
            continue;
        };
        if a == 0 {
            continue;
        }
        n = n.saturating_add(1);
        for (i, v) in [r, g, b].into_iter().enumerate() {
            let v = f64::from(v);
            if let (Some(s), Some(q)) = (sum.get_mut(i), sq.get_mut(i)) {
                *s += v;
                *q += v * v;
            }
        }
        let lum = (0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b)) / 255.0;
        lum_sum += lum;
        lum_min = lum_min.min(lum);
        lum_max = lum_max.max(lum);
    }
    if n == 0 {
        return Stats {
            mean: [0, 0, 0],
            std_dev: 0.0,
            coverage: 0.0,
            luminance: 0.0,
            lum_min: 0.0,
            lum_max: 0.0,
        };
    }
    let nf = n as f64;
    let mean = sum.map(|s| s / nf);
    let var: f64 = (0..3)
        .map(|i| {
            let m = mean.get(i).copied().unwrap_or(0.0);
            (sq.get(i).copied().unwrap_or(0.0) / nf - m * m).max(0.0)
        })
        .sum::<f64>()
        / 3.0;
    Stats {
        mean: mean.map(|m| m.round().clamp(0.0, 255.0) as u8),
        std_dev: var.sqrt() as f32,
        coverage: (nf / seen.max(1) as f64) as f32,
        luminance: (lum_sum / nf) as f32,
        lum_min: lum_min as f32,
        lum_max: lum_max as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_colour_has_no_spread_and_its_own_mean() {
        let rgba = [0u8, 0x90, 0xFF, 255].repeat(100);
        let s = measure(&rgba);
        assert_eq!(s.mean, [0, 0x90, 0xFF]);
        assert!(s.std_dev < 0.01);
        assert!((s.coverage - 1.0).abs() < 1e-6);
    }

    #[test]
    fn clear_pixels_only_lower_the_coverage() {
        let mut rgba = [255u8, 255, 255, 255].repeat(3);
        rgba.extend([0u8, 0, 0, 0]);
        let s = measure(&rgba);
        assert_eq!(s.mean, [255, 255, 255]);
        assert!((s.coverage - 0.75).abs() < 1e-6);
        assert!((s.luminance - 1.0).abs() < 1e-6);
    }
}
