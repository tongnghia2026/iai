#![allow(dead_code)]
//! PathData → RGBA raster cache (foundation-adjacent, Bước 4 minimal / T5.1 full).
//!
//! The vector model ([`VectorObjectData`]) is the source of truth; this produces
//! the derived `Layer::tiles` raster the existing compositor, thumbnail and
//! raster export consume (Mục 3.2 / 3.8). It is a straight-alpha CPU rasteriser
//! with no UI/GPU dependency, mirroring the synchronous `ShapeData::render`
//! precedent (Mục 3.5 allows a synchronous fallback in the first slice).
//!
//! This is deliberately the *minimal* correct rasteriser for Bước 4: an analytic
//! scanline fill (vertical supersampling + horizontal analytic coverage, honouring
//! the path's fill rule) plus a per-segment capsule stroke. The generation /
//! stale-worker / dirty-rect / GPU-atlas machinery is Bước 5 (T5.2) and is layered
//! ON TOP without changing this function's output.

use crate::core::geometry::Point;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::flatten::flatten_path;
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::style::{GradientKind, Paint};

/// Flatten tolerance in layer pixels. Small enough to be invisible at 100%, large
/// enough that a page-sized curve does not explode the polyline.
const FLATTEN_TOL: f32 = 0.25;
/// Vertical fill subsamples per output row (horizontal is analytic).
const FILL_SUBSAMPLES: u32 = 4;
/// Refuse rasters larger than this many pixels in the first slice; the model
/// still round-trips and B5's tiled rasteriser will remove the ceiling.
const MAX_RASTER_PIXELS: u64 = 64_000_000;

/// A rendered vector object: a tight RGBA buffer plus the integer offset that
/// places its top-left in the object's coordinate (layer) space.
pub struct PathRaster {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub offset: (i32, i32),
}

/// Rasterise a vector object into a tight straight-alpha RGBA buffer. Returns
/// `None` when there is nothing visible to draw (empty path, no fill and no
/// outline) or the raster would exceed [`MAX_RASTER_PIXELS`].
pub fn rasterize(object: &VectorObjectData) -> Option<PathRaster> {
    rasterize_impl(object, None)
}

/// Rasterize only the intersection with `clip` in layer coordinates. This is
/// used by the high-zoom editor overlay so a 6400% view allocates roughly the
/// visible screen, not a 64x copy of the entire object.
pub fn rasterize_clipped(
    object: &VectorObjectData,
    clip: crate::core::geometry::Rect,
) -> Option<PathRaster> {
    rasterize_impl(object, Some(clip))
}

/// Flattened contours plus the derived raster frame, shared by [`rasterize`]
/// and [`raster_geometry`] so both compute the identical placement.
struct RasterLayout {
    polylines: Vec<Vec<Point>>,
    closed: Vec<bool>,
    off_x: f32,
    off_y: f32,
    w: u32,
    h: u32,
}

fn raster_layout(
    object: &VectorObjectData,
    clip: Option<crate::core::geometry::Rect>,
) -> Option<RasterLayout> {
    let fill_visible = object.style.fill.is_visible();
    let half = object.style.effective_stroke_width() * 0.5;
    let stroke_visible = object.style.stroke.is_visible() && half > 0.0;
    if !fill_visible && !stroke_visible {
        return None;
    }

    let lpath = object.path_in_layer_space();
    // Per-contour polylines in LAYER space, with the source `closed` flag so the
    // stroke does not draw a phantom closing edge on an open contour.
    let polylines = flatten_path(&lpath, FLATTEN_TOL);
    let closed: Vec<bool> = lpath.contours.iter().map(|c| c.closed).collect();

    // Bounds over every on-curve point.
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for pl in &polylines {
        for p in pl {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
    }
    if !min_x.is_finite() || max_x < min_x {
        return None;
    }

    // AA margin: half the stroke plus a pixel for the coverage ramp.
    let pad = half.max(0.0) + 1.0;
    let mut off_x = (min_x - pad).floor();
    let mut off_y = (min_y - pad).floor();
    let mut end_x = (max_x + pad).ceil();
    let mut end_y = (max_y + pad).ceil();
    if let Some(clip) = clip {
        off_x = off_x.max(clip.x.floor());
        off_y = off_y.max(clip.y.floor());
        end_x = end_x.min(clip.right().ceil());
        end_y = end_y.min(clip.bottom().ceil());
        if end_x <= off_x || end_y <= off_y {
            return None;
        }
    }
    let w = (end_x - off_x).max(1.0);
    let h = (end_y - off_y).max(1.0);
    if (w as u64) * (h as u64) > MAX_RASTER_PIXELS {
        return None;
    }
    Some(RasterLayout {
        polylines,
        closed,
        off_x,
        off_y,
        w: w as u32,
        h: h as u32,
    })
}

/// The exact placement [`rasterize`] would produce for `object` —
/// `(offset, width, height)` — from the flattened bounds alone, WITHOUT filling
/// any pixels: O(nodes), not O(area). `None` exactly when [`rasterize`] returns
/// `None`. Lets a caller detect an already-in-sync raster cache cheaply instead
/// of re-rendering the whole object just to learn its origin.
pub fn raster_geometry(object: &VectorObjectData) -> Option<((i32, i32), u32, u32)> {
    let l = raster_layout(object, None)?;
    Some(((l.off_x as i32, l.off_y as i32), l.w, l.h))
}

fn rasterize_impl(
    object: &VectorObjectData,
    clip: Option<crate::core::geometry::Rect>,
) -> Option<PathRaster> {
    let RasterLayout {
        polylines,
        closed,
        off_x,
        off_y,
        w,
        h,
    } = raster_layout(object, clip)?;
    let fill_visible = object.style.fill.is_visible();
    let half = object.style.effective_stroke_width() * 0.5;
    let stroke_visible = object.style.stroke.is_visible() && half > 0.0;
    let offset = (off_x as i32, off_y as i32);

    // Shift every polyline into raster-local space once.
    let local: Vec<Vec<Point>> = polylines
        .iter()
        .map(|pl| {
            pl.iter()
                .map(|p| Point::new(p.x - off_x, p.y - off_y))
                .collect()
        })
        .collect();

    let opacity = object.style.opacity.clamp(0.0, 1.0);
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];

    // Painting a coverage mask is embarrassingly parallel by row, and every
    // per-raster constant (affine inverses, stop table) is hoisted into
    // `PreparedPaint` — the naive loop recomputed two matrix inverses per pixel,
    // which made a page-sized gradient fill the dominant cost of a raster.
    let paint_rows = |rgba: &mut [u8], cov: &[f32], paint: &PreparedPaint| {
        use rayon::prelude::*;
        let wu = w as usize;
        rgba.par_chunks_mut(wu * 4)
            .enumerate()
            .for_each(|(py, row)| {
                let y = py as f32 + off_y + 0.5;
                let cov_row = &cov[py * wu..(py + 1) * wu];
                for (px_i, px) in row.chunks_exact_mut(4).enumerate() {
                    let c = cov_row[px_i];
                    if c > 0.0015 {
                        let [r, g, b, a] = paint.sample(Point::new(px_i as f32 + off_x + 0.5, y));
                        let mut p = [px[0], px[1], px[2], px[3]];
                        blend_straight(
                            &mut p,
                            [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0],
                            (a as f32 / 255.0) * opacity * c,
                        );
                        px.copy_from_slice(&p);
                    }
                }
            });
    };

    // ── Fill ─────────────────────────────────────────────────────────────────
    if fill_visible {
        let even_odd = object.path.fill_rule == crate::core::vector::path::FillRule::EvenOdd;
        let cov = fill_coverage(&local, w, h, even_odd);
        paint_rows(
            &mut rgba,
            &cov,
            &PreparedPaint::new(object.style.fill, object),
        );
    }

    // ── Stroke (over the fill) ───────────────────────────────────────────────
    if stroke_visible {
        let (stroke_lines, stroke_closed) = dashed_polylines(
            &local,
            &closed,
            object.style.stroke_style.dash.as_slice(),
            object.style.stroke_style.dash.offset,
        );
        let cov = stroke_coverage(&stroke_lines, &stroke_closed, w, h, half);
        paint_rows(
            &mut rgba,
            &cov,
            &PreparedPaint::new(object.style.stroke, object),
        );
    }

    Some(PathRaster {
        rgba,
        width: w,
        height: h,
        offset,
    })
}

/// A paint with its per-raster constants resolved once: the object/gradient
/// inverses collapsed into a single LAYER→gradient transform and the stops
/// pre-converted to RGBA8. `sample` is then pure per-pixel arithmetic.
enum PreparedPaint {
    None,
    Solid([u8; 4]),
    Gradient {
        /// LAYER space → gradient space (`gradient⁻¹ ∘ object⁻¹`).
        to_gradient: AffineTransform,
        radial: bool,
        stops: Vec<(f32, [u8; 4])>,
    },
}

impl PreparedPaint {
    fn new(paint: Paint, object: &VectorObjectData) -> Self {
        // A singular object transform blanks every paint, matching the
        // per-pixel `sample_paint` this replaces.
        let Some(object_inv) = object.transform.inverse() else {
            return Self::None;
        };
        match paint {
            Paint::None => Self::None,
            Paint::Solid(color) => Self::Solid(color.to_rgba8()),
            Paint::Gradient(gradient) => {
                let Some(gradient_inv) = gradient.transform.inverse() else {
                    return Self::None;
                };
                Self::Gradient {
                    to_gradient: gradient_inv.then(&object_inv),
                    radial: gradient.kind == GradientKind::Radial,
                    stops: gradient
                        .active_stops()
                        .iter()
                        .map(|s| (s.offset, s.color.to_rgba8()))
                        .collect(),
                }
            }
        }
    }

    fn sample(&self, layer_point: Point) -> [u8; 4] {
        match self {
            Self::None => [0, 0, 0, 0],
            Self::Solid(color) => *color,
            Self::Gradient {
                to_gradient,
                radial,
                stops,
            } => {
                let gp = to_gradient.apply_point(layer_point);
                let t = if *radial {
                    (gp.x * gp.x + gp.y * gp.y).sqrt()
                } else {
                    gp.x
                }
                .clamp(0.0, 1.0);
                let right = stops
                    .iter()
                    .position(|s| s.0 >= t)
                    .unwrap_or(stops.len() - 1);
                if right == 0 {
                    return stops[0].1;
                }
                let (a_off, ca) = stops[right - 1];
                let (b_off, cb) = stops[right];
                let mix = ((t - a_off) / (b_off - a_off).max(1e-6)).clamp(0.0, 1.0);
                let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * mix).round() as u8;
                [
                    lerp(ca[0], cb[0]),
                    lerp(ca[1], cb[1]),
                    lerp(ca[2], cb[2]),
                    lerp(ca[3], cb[3]),
                ]
            }
        }
    }
}

/// Sample a vector paint at an object-local point. Parametric primitives use
/// this same function, so Path and Shape gradients have identical stop,
/// transform and alpha semantics.
pub(crate) fn sample_paint_in_object_space(paint: Paint, object_point: Point) -> [u8; 4] {
    match paint {
        Paint::None => [0, 0, 0, 0],
        Paint::Solid(color) => color.to_rgba8(),
        Paint::Gradient(gradient) => {
            let Some(gradient_inv) = gradient.transform.inverse() else {
                return [0, 0, 0, 0];
            };
            let gp = gradient_inv.apply_point(object_point);
            let t = match gradient.kind {
                GradientKind::Linear => gp.x,
                GradientKind::Radial => (gp.x * gp.x + gp.y * gp.y).sqrt(),
            }
            .clamp(0.0, 1.0);
            let stops = gradient.active_stops();
            let right = stops
                .iter()
                .position(|s| s.offset >= t)
                .unwrap_or(stops.len() - 1);
            if right == 0 {
                return stops[0].color.to_rgba8();
            }
            let a = stops[right - 1];
            let b = stops[right];
            let mix = ((t - a.offset) / (b.offset - a.offset).max(1e-6)).clamp(0.0, 1.0);
            let ca = a.color.to_rgba8();
            let cb = b.color.to_rgba8();
            let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * mix).round() as u8;
            [
                lerp(ca[0], cb[0]),
                lerp(ca[1], cb[1]),
                lerp(ca[2], cb[2]),
                lerp(ca[3], cb[3]),
            ]
        }
    }
}

fn dashed_polylines(
    input: &[Vec<Point>],
    closed: &[bool],
    pattern: &[f32],
    offset: f32,
) -> (Vec<Vec<Point>>, Vec<bool>) {
    if pattern.is_empty() {
        return (input.to_vec(), closed.to_vec());
    }
    let mut pat = pattern.to_vec();
    if pat.len() % 2 == 1 {
        pat.extend_from_within(..);
    }
    let period: f32 = pat.iter().sum();
    if period <= f32::EPSILON {
        return (input.to_vec(), closed.to_vec());
    }
    let mut out = Vec::new();
    for (ci, line) in input.iter().enumerate() {
        if line.len() < 2 {
            continue;
        }
        let mut phase = offset.rem_euclid(period);
        let mut pi = 0usize;
        while phase >= pat[pi] {
            phase -= pat[pi];
            pi = (pi + 1) % pat.len();
        }
        let count = if closed.get(ci).copied().unwrap_or(false) {
            line.len()
        } else {
            line.len() - 1
        };
        for si in 0..count {
            let a = line[si];
            let b = line[(si + 1) % line.len()];
            let dx = b.x - a.x;
            let dy = b.y - a.y;
            let length = (dx * dx + dy * dy).sqrt();
            if length <= 1e-6 {
                continue;
            }
            let mut at = 0.0;
            while at < length {
                let take = (pat[pi] - phase).min(length - at);
                if pi % 2 == 0 && take > 1e-6 {
                    let p0 = Point::new(a.x + dx * (at / length), a.y + dy * (at / length));
                    let p1 = Point::new(
                        a.x + dx * ((at + take) / length),
                        a.y + dy * ((at + take) / length),
                    );
                    out.push(vec![p0, p1]);
                }
                at += take;
                phase += take;
                if phase + 1e-6 >= pat[pi] {
                    phase = 0.0;
                    pi = (pi + 1) % pat.len();
                }
            }
        }
    }
    let flags = vec![false; out.len()];
    (out, flags)
}

/// Analytic scanline fill coverage in `[0,1]` per pixel. Each contour is treated
/// as closed (an open contour is implicitly closed for filling, matching every
/// 2D vector renderer). Vertical antialiasing comes from [`FILL_SUBSAMPLES`]
/// sub-scanlines; horizontal antialiasing is analytic at the span endpoints.
fn fill_coverage(local: &[Vec<Point>], w: u32, h: u32, even_odd: bool) -> Vec<f32> {
    use rayon::prelude::*;

    let mut cov = vec![0f32; (w as usize) * (h as usize)];
    // Edge list: (x0,y0,x1,y1) with y0 < y1 tracked via winding sign.
    struct Edge {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        dir: i32,
    }
    let mut edges: Vec<Edge> = Vec::new();
    for pl in local {
        if pl.len() < 2 {
            continue;
        }
        let n = pl.len();
        for i in 0..n {
            let a = pl[i];
            // Close the ring: the last→first edge fills an open contour too.
            let b = pl[(i + 1) % n];
            if (a.y - b.y).abs() < f32::EPSILON {
                continue; // horizontal edges contribute no crossings
            }
            let (x0, y0, x1, y1, dir) = if a.y < b.y {
                (a.x, a.y, b.x, b.y, 1)
            } else {
                (b.x, b.y, a.x, a.y, -1)
            };
            edges.push(Edge {
                x0,
                y0,
                x1,
                y1,
                dir,
            });
        }
    }
    if edges.is_empty() {
        return cov;
    }

    let ss = FILL_SUBSAMPLES;
    let sub_w = 1.0 / ss as f32;
    let wu = w as usize;
    // Each output row is independent (it reads the shared edge list and writes
    // only its own `w` pixels), so scanlines run in parallel — filling a
    // page-sized path is O(area) and was the drag/colour-picker bottleneck. The
    // per-row result is byte-identical to the serial scan.
    cov.par_chunks_mut(wu).enumerate().for_each(|(py, row)| {
        let mut xs: Vec<(f32, i32)> = Vec::new();
        for s in 0..ss {
            let yc = py as f32 + (s as f32 + 0.5) * sub_w;
            xs.clear();
            for e in &edges {
                if yc >= e.y0 && yc < e.y1 {
                    let t = (yc - e.y0) / (e.y1 - e.y0);
                    xs.push((e.x0 + t * (e.x1 - e.x0), e.dir));
                }
            }
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let mut wind = 0i32;
            for pair in 0..xs.len() - 1 {
                wind += xs[pair].1;
                let inside = if even_odd {
                    // Parity of the number of crossings seen so far.
                    (pair as i32 + 1) & 1 == 1
                } else {
                    wind != 0
                };
                if inside {
                    add_span(row, xs[pair].0, xs[pair + 1].0, sub_w);
                }
            }
        }
    });
    cov
}

/// Add `weight * horizontal_overlap` to each pixel the interval `[xa, xb)` covers,
/// with analytic partial coverage at the two endpoints.
fn add_span(row: &mut [f32], xa: f32, xb: f32, weight: f32) {
    let w = row.len();
    let xa = xa.max(0.0);
    let xb = xb.min(w as f32);
    if xb <= xa {
        return;
    }
    let start = xa.floor() as usize;
    let end = (xb.ceil() as usize).min(w);
    for x in start..end {
        let l = (x as f32).max(xa);
        let r = ((x + 1) as f32).min(xb);
        let o = (r - l).clamp(0.0, 1.0);
        if o > 0.0 {
            row[x] = (row[x] + weight * o).min(1.0);
        }
    }
}

/// Per-segment capsule stroke coverage in `[0,1]` per pixel, unioned via max so
/// overlapping segments/joins stay opaque without double-counting. Only pixels
/// inside each segment's expanded bbox are touched, so a page-sized thin outline
/// costs O(perimeter·width), not O(area).
fn stroke_coverage(local: &[Vec<Point>], closed: &[bool], w: u32, h: u32, half: f32) -> Vec<f32> {
    let mut cov = vec![0f32; (w as usize) * (h as usize)];
    let reach = half + 0.5; // coverage ramp reaches half a pixel past the edge
    for (ci, pl) in local.iter().enumerate() {
        if pl.len() < 2 {
            continue;
        }
        let n = pl.len();
        // A closed flattened contour already ends at its start point, so edges
        // 0..n-1 cover the ring; an open one stops before wrapping.
        let seg_count = if closed.get(ci).copied().unwrap_or(false) {
            n // includes the wrap edge (pl[n-1] == pl[0] for a closed ring)
        } else {
            n - 1
        };
        for i in 0..seg_count {
            let a = pl[i];
            let b = pl[(i + 1) % n];
            let min_x = (a.x.min(b.x) - reach).floor().max(0.0) as u32;
            let max_x = ((a.x.max(b.x) + reach).ceil() as i64).clamp(0, w as i64) as u32;
            let min_y = (a.y.min(b.y) - reach).floor().max(0.0) as u32;
            let max_y = ((a.y.max(b.y) + reach).ceil() as i64).clamp(0, h as i64) as u32;
            for py in min_y..max_y {
                let row = (py as usize) * (w as usize);
                for px in min_x..max_x {
                    let d = dist_to_segment(px as f32 + 0.5, py as f32 + 0.5, a, b);
                    let c = (0.5 - (d - half)).clamp(0.0, 1.0);
                    if c > 0.0 {
                        let idx = row + px as usize;
                        if c > cov[idx] {
                            cov[idx] = c;
                        }
                    }
                }
            }
        }
    }
    cov
}

/// Distance from `(px,py)` to segment `a→b`.
fn dist_to_segment(px: f32, py: f32, a: Point, b: Point) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-6 {
        let ex = px - a.x;
        let ey = py - a.y;
        return (ex * ex + ey * ey).sqrt();
    }
    let t = (((px - a.x) * dx + (py - a.y) * dy) / len2).clamp(0.0, 1.0);
    let cx = a.x + t * dx;
    let cy = a.y + t * dy;
    let ex = px - cx;
    let ey = py - cy;
    (ex * ex + ey * ey).sqrt()
}

/// Source-over blend of a straight-alpha colour into an `[r,g,b,a]` pixel (u8),
/// matching `shape.rs`'s blend so vector and shape rasters composite identically.
fn blend_straight(px: &mut [u8; 4], src: [f32; 3], src_a: f32) {
    if src_a <= 0.0015 {
        return;
    }
    let dr = px[0] as f32 / 255.0;
    let dg = px[1] as f32 / 255.0;
    let db = px[2] as f32 / 255.0;
    let da = px[3] as f32 / 255.0;
    let a = src_a.min(1.0);
    let out_a = a + da * (1.0 - a);
    if out_a <= 0.0015 {
        return;
    }
    let inv = da * (1.0 - a);
    px[0] = (((src[0] * a + dr * inv) / out_a) * 255.0).round() as u8;
    px[1] = (((src[1] * a + dg * inv) / out_a) * 255.0).round() as u8;
    px[2] = (((src[2] * a + db * inv) / out_a) * 255.0).round() as u8;
    px[3] = (out_a * 255.0).round() as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vector::affine::AffineTransform;
    use crate::core::vector::color::ColorValue;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};
    use crate::core::vector::style::VectorStyle;
    use crate::core::vector::style::{DashPattern, Gradient, GradientKind, Paint};

    fn square_obj(side: f32, style: VectorStyle) -> VectorObjectData {
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(side, 0.0)),
                    Node::sharp(Point::new(side, side)),
                    Node::sharp(Point::new(0.0, side)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        VectorObjectData::new(path, style, AffineTransform::translate(20.0, 20.0))
    }

    #[test]
    fn dashed_stroke_has_visible_gaps() {
        let mut style = VectorStyle::stroked(ColorValue::BLACK, 4.0);
        style.stroke_style.dash = DashPattern::from_slice(&[8.0, 8.0], 0.0);
        let raster = rasterize(&square_obj(80.0, style)).unwrap();
        let opaque = raster.rgba.chunks_exact(4).filter(|p| p[3] > 200).count();
        let solid = rasterize(&square_obj(
            80.0,
            VectorStyle::stroked(ColorValue::BLACK, 4.0),
        ))
        .unwrap();
        let solid_opaque = solid.rgba.chunks_exact(4).filter(|p| p[3] > 200).count();
        assert!(opaque < solid_opaque);
    }

    #[test]
    fn linear_gradient_changes_colour_across_fill() {
        let gradient = Gradient::two_color(
            GradientKind::Linear,
            ColorValue::BLACK,
            ColorValue::WHITE,
            AffineTransform::scale(40.0, 40.0),
        );
        let obj = square_obj(
            40.0,
            VectorStyle {
                fill: Paint::Gradient(gradient),
                ..VectorStyle::default()
            },
        );
        let raster = rasterize(&obj).unwrap();
        let y = raster.height as usize / 2;
        let left = (y * raster.width as usize + 5) * 4;
        let right = (y * raster.width as usize + raster.width as usize - 6) * 4;
        assert!(raster.rgba[left] + 80 < raster.rgba[right]);
    }

    fn alpha_at(r: &PathRaster, x: u32, y: u32) -> u8 {
        r.rgba[((y * r.width + x) * 4 + 3) as usize]
    }

    #[test]
    fn empty_style_renders_nothing() {
        let mut style = VectorStyle::default();
        style.fill = Paint::None;
        style.stroke = Paint::None;
        assert!(rasterize(&square_obj(40.0, style)).is_none());
    }

    /// `raster_geometry` must report exactly the frame `rasterize` produces —
    /// it is the cheap in-sync check that lets a gesture press skip the O(area)
    /// re-raster.
    #[test]
    fn raster_geometry_matches_rasterize_frame() {
        let gradient = Gradient::two_color(
            GradientKind::Linear,
            ColorValue::BLACK,
            ColorValue::WHITE,
            AffineTransform::scale(40.0, 40.0),
        );
        let cases = [
            square_obj(40.0, VectorStyle::filled(ColorValue::rgb(0.0, 1.0, 0.0))),
            square_obj(40.0, VectorStyle::stroked(ColorValue::BLACK, 6.0)),
            square_obj(
                40.0,
                VectorStyle {
                    fill: Paint::Gradient(gradient),
                    ..VectorStyle::default()
                },
            ),
        ];
        for obj in cases {
            let r = rasterize(&obj).expect("raster");
            let (offset, w, h) = raster_geometry(&obj).expect("geometry");
            assert_eq!(offset, r.offset);
            assert_eq!((w, h), (r.width, r.height));
        }
        let mut invisible = VectorStyle::default();
        invisible.fill = Paint::None;
        invisible.stroke = Paint::None;
        assert!(raster_geometry(&square_obj(40.0, invisible)).is_none());
    }

    #[test]
    fn filled_square_is_opaque_inside_offset_placed() {
        let r = rasterize(&square_obj(
            40.0,
            VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
        ))
        .expect("raster");
        // Offset places the raster near (20,20) minus the AA pad.
        assert!(r.offset.0 <= 20 && r.offset.1 <= 20);
        // Centre of the square (~ layer (40,40)) is opaque red.
        let cx = (40 - r.offset.0) as u32;
        let cy = (40 - r.offset.1) as u32;
        let i = ((cy * r.width + cx) * 4) as usize;
        assert!(r.rgba[i + 3] > 250, "interior opaque");
        assert_eq!(&r.rgba[i..i + 3], &[255, 0, 0], "fill colour");
        // A corner of the raster is transparent (outside the square).
        assert_eq!(alpha_at(&r, 0, 0), 0, "outside transparent");
    }

    #[test]
    fn stroke_only_leaves_interior_hollow() {
        let r = rasterize(&square_obj(
            40.0,
            VectorStyle::stroked(ColorValue::BLACK, 4.0),
        ))
        .expect("raster");
        // The centre is well inside; a stroke-only object leaves it transparent.
        let cx = (40 - r.offset.0) as u32;
        let cy = (40 - r.offset.1) as u32;
        assert_eq!(alpha_at(&r, cx, cy), 0, "interior hollow for stroke-only");
        // A point on the left edge (layer x≈20) is painted.
        let ex = (20 - r.offset.0) as u32;
        assert!(alpha_at(&r, ex, cy) > 200, "edge painted");
    }

    #[test]
    fn opacity_scales_alpha() {
        let mut style = VectorStyle::filled(ColorValue::rgb(0.0, 0.0, 0.0));
        style.opacity = 0.5;
        let r = rasterize(&square_obj(40.0, style)).expect("raster");
        let cx = (40 - r.offset.0) as u32;
        let cy = (40 - r.offset.1) as u32;
        let a = alpha_at(&r, cx, cy);
        assert!(
            (a as i32 - 128).abs() <= 3,
            "opacity 0.5 → alpha ~128, got {a}"
        );
    }
}
