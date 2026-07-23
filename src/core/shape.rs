// Vector shape layer data + rasteriser.
//
// A Shape layer stores its `ShapeData` (kind + geometry + fill/stroke style) and
// re-renders into the layer's tiles whenever it is created or edited — the same
// non-destructive pattern as a Text layer. Geometry is kept in *layer-local*
// coordinates (0,0 = the layer raster's top-left); `Layer::offset` positions the
// raster on the canvas, so the Move tool can shift the layer without touching the
// geometry, and editing handles work in canvas space via `canvas_span`.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
    Line,
    /// Regular N-gon inscribed in the bounding box (`sides` = number of edges).
    Polygon,
    /// N-pointed star inscribed in the box (`sides` = points, `star_inner` = inner
    /// radius as a fraction of the outer).
    Star,
}

impl ShapeKind {
    pub fn from_u8(v: u8) -> ShapeKind {
        match v {
            1 => ShapeKind::Ellipse,
            2 => ShapeKind::Line,
            3 => ShapeKind::Polygon,
            4 => ShapeKind::Star,
            _ => ShapeKind::Rectangle,
        }
    }
    pub fn to_u8(self) -> u8 {
        match self {
            ShapeKind::Rectangle => 0,
            ShapeKind::Ellipse => 1,
            ShapeKind::Line => 2,
            ShapeKind::Polygon => 3,
            ShapeKind::Star => 4,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ShapeKind::Rectangle => "Rectangle",
            ShapeKind::Ellipse => "Ellipse",
            ShapeKind::Line => "Line",
            ShapeKind::Polygon => "Polygon",
            ShapeKind::Star => "Star",
        }
    }
    /// A box-bounded, fillable shape (Rectangle / Ellipse / Polygon / Star) — as
    /// opposed to the open Line. Used to share box handling across kinds.
    pub fn is_box_shape(self) -> bool {
        !matches!(self, ShapeKind::Line)
    }
}

/// On-canvas edit handles. The eight box handles resize the bounding box; the
/// radius node drags the rounded-rectangle corner radius; Line uses its two
/// endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    Radius,
    LineStart,
    LineEnd,
}

impl ShapeHandle {
    pub fn to_u8(self) -> u8 {
        match self {
            ShapeHandle::TopLeft => 0,
            ShapeHandle::Top => 1,
            ShapeHandle::TopRight => 2,
            ShapeHandle::Right => 3,
            ShapeHandle::BottomRight => 4,
            ShapeHandle::Bottom => 5,
            ShapeHandle::BottomLeft => 6,
            ShapeHandle::Left => 7,
            ShapeHandle::Radius => 8,
            ShapeHandle::LineStart => 9,
            ShapeHandle::LineEnd => 10,
        }
    }
    pub fn from_u8(v: u8) -> ShapeHandle {
        match v {
            1 => ShapeHandle::Top,
            2 => ShapeHandle::TopRight,
            3 => ShapeHandle::Right,
            4 => ShapeHandle::BottomRight,
            5 => ShapeHandle::Bottom,
            6 => ShapeHandle::BottomLeft,
            7 => ShapeHandle::Left,
            8 => ShapeHandle::Radius,
            9 => ShapeHandle::LineStart,
            10 => ShapeHandle::LineEnd,
            _ => ShapeHandle::TopLeft,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShapeData {
    pub kind: ShapeKind,
    /// Geometry in layer-local coordinates. Rectangle/Ellipse: two opposite
    /// bounding-box corners. Line: the two endpoints.
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Rounded-rectangle corner radius (canvas px). 0 = sharp.
    pub corner_radius: f32,
    /// Polygon edge count / Star point count (ignored for other kinds).
    pub sides: u32,
    /// Star inner-radius fraction of the outer radius, in `(0,1)` (Star only).
    pub star_inner: f32,
    pub fill: bool,
    pub fill_color: [u8; 4],
    /// Outline / line thickness (canvas px). 0 = no outline.
    pub stroke_width: f32,
    pub stroke_color: [u8; 4],
}

/// A rendered shape: tight RGBA buffer sized to the shape's own extent.
pub struct ShapeRaster {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl ShapeData {
    /// Padding added around the geometry so the anti-aliased stroke edge is not
    /// clipped by the raster bounds.
    fn pad(stroke_width: f32) -> f32 {
        stroke_width.max(1.0) * 0.5 + 2.0
    }

    /// Build a shape from a canvas-space drag span, returning the shape (local
    /// geometry) and the integer layer offset that places it. Orientation is
    /// preserved for lines; rectangles/ellipses use the span as opposite corners.
    #[allow(clippy::too_many_arguments)]
    pub fn from_canvas_span(
        kind: ShapeKind,
        mut cx0: f32,
        mut cy0: f32,
        mut cx1: f32,
        mut cy1: f32,
        corner_radius: f32,
        fill: bool,
        fill_color: [u8; 4],
        stroke_width: f32,
        stroke_color: [u8; 4],
    ) -> (ShapeData, (i32, i32)) {
        (cx0, cy0, cx1, cy1) =
            pixel_snapped_span(kind, cx0, cy0, cx1, cy1, fill, stroke_width, stroke_color);
        let pad = Self::pad(stroke_width);
        let minx = cx0.min(cx1);
        let miny = cy0.min(cy1);
        let off_x = (minx - pad).floor();
        let off_y = (miny - pad).floor();
        let data = ShapeData {
            kind,
            x0: cx0 - off_x,
            y0: cy0 - off_y,
            x1: cx1 - off_x,
            y1: cy1 - off_y,
            corner_radius: corner_radius.max(0.0),
            // Defaults; Polygon/Star callers overwrite these (kept out of the
            // signature so existing call sites are unchanged). `rebuilt` copies
            // them from `self`, so a resize never resets them.
            sides: 5,
            star_inner: 0.5,
            fill,
            fill_color,
            stroke_width: stroke_width.max(0.0),
            stroke_color,
        };
        (data, (off_x as i32, off_y as i32))
    }

    /// The shape's geometry in canvas space, given its layer offset.
    pub fn canvas_span(&self, offset: (i32, i32)) -> (f32, f32, f32, f32) {
        (
            self.x0 + offset.0 as f32,
            self.y0 + offset.1 as f32,
            self.x1 + offset.0 as f32,
            self.y1 + offset.1 as f32,
        )
    }

    /// Canvas-space positions of the resize handles (box corners/edges, or the
    /// two line endpoints). The rounded-corner radius node is positioned by the
    /// caller since it depends on the view zoom.
    pub fn handle_points(&self, offset: (i32, i32)) -> Vec<(ShapeHandle, f32, f32)> {
        let (x0, y0, x1, y1) = self.canvas_span(offset);
        if self.kind == ShapeKind::Line {
            return vec![
                (ShapeHandle::LineStart, x0, y0),
                (ShapeHandle::LineEnd, x1, y1),
            ];
        }
        let minx = x0.min(x1);
        let maxx = x0.max(x1);
        let miny = y0.min(y1);
        let maxy = y0.max(y1);
        let midx = (minx + maxx) * 0.5;
        let midy = (miny + maxy) * 0.5;
        vec![
            (ShapeHandle::TopLeft, minx, miny),
            (ShapeHandle::Top, midx, miny),
            (ShapeHandle::TopRight, maxx, miny),
            (ShapeHandle::Right, maxx, midy),
            (ShapeHandle::BottomRight, maxx, maxy),
            (ShapeHandle::Bottom, midx, maxy),
            (ShapeHandle::BottomLeft, minx, maxy),
            (ShapeHandle::Left, minx, midy),
        ]
    }

    /// Effective corner radius, clamped to half the shorter side.
    pub fn effective_radius(&self) -> f32 {
        let w = (self.x1 - self.x0).abs();
        let h = (self.y1 - self.y0).abs();
        self.corner_radius.min(w * 0.5).min(h * 0.5).max(0.0)
    }

    /// Local-space vertices of a Polygon (N sharp corners) or Star (2N alternating
    /// corners) inscribed in the bounding box — first vertex points up. Empty for
    /// non-polygonal kinds. The box's half-extents are the radii, so the shape
    /// stretches with a non-square box.
    pub fn polygon_vertices(&self) -> Vec<(f32, f32)> {
        let cx = (self.x0 + self.x1) * 0.5;
        let cy = (self.y0 + self.y1) * 0.5;
        let rx = (self.x1 - self.x0).abs() * 0.5;
        let ry = (self.y1 - self.y0).abs() * 0.5;
        let n = self.sides.clamp(3, 200) as usize;
        let start = -std::f32::consts::FRAC_PI_2;
        match self.kind {
            ShapeKind::Polygon => (0..n)
                .map(|i| {
                    let a = start + std::f32::consts::TAU * i as f32 / n as f32;
                    (cx + rx * a.cos(), cy + ry * a.sin())
                })
                .collect(),
            ShapeKind::Star => {
                let inner = self.star_inner.clamp(0.05, 0.95);
                (0..2 * n)
                    .map(|i| {
                        let a = start + std::f32::consts::PI * i as f32 / n as f32;
                        let f = if i % 2 == 0 { 1.0 } else { inner };
                        (cx + rx * f * a.cos(), cy + ry * f * a.sin())
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// Size of the tight raster that holds this shape.
    pub fn raster_size(&self) -> (u32, u32) {
        let pad = Self::pad(self.stroke_width);
        let maxx = self.x0.max(self.x1);
        let maxy = self.y0.max(self.y1);
        let w = (maxx + pad).ceil().max(1.0);
        let h = (maxy + pad).ceil().max(1.0);
        (w as u32, h as u32)
    }

    /// Re-derive the shape from an edited canvas-space span, keeping style.
    fn rebuilt(&self, cx0: f32, cy0: f32, cx1: f32, cy1: f32) -> (ShapeData, (i32, i32)) {
        let (mut data, off) = Self::from_canvas_span(
            self.kind,
            cx0,
            cy0,
            cx1,
            cy1,
            self.corner_radius,
            self.fill,
            self.fill_color,
            self.stroke_width,
            self.stroke_color,
        );
        // Corner radius can't exceed half the shorter side.
        let w = (cx1 - cx0).abs();
        let h = (cy1 - cy0).abs();
        data.corner_radius = data.corner_radius.min(w * 0.5).min(h * 0.5).max(0.0);
        // Polygon/Star parameters are not derived from the span — carry them over.
        data.sides = self.sides;
        data.star_inner = self.star_inner;
        (data, off)
    }

    /// Apply a handle drag to a new canvas point, returning the edited shape and
    /// its new layer offset. `keep_ratio` (Shift) constrains box handles to the
    /// original aspect / lines to 45°.
    pub fn resize_by_handle(
        &self,
        offset: (i32, i32),
        handle: ShapeHandle,
        cx: f32,
        cy: f32,
    ) -> (ShapeData, (i32, i32)) {
        let (sx0, sy0, sx1, sy1) = self.canvas_span(offset);
        match handle {
            ShapeHandle::LineStart => self.rebuilt(cx, cy, sx1, sy1),
            ShapeHandle::LineEnd => self.rebuilt(sx0, sy0, cx, cy),
            ShapeHandle::Radius => {
                // Node rides the top edge inset from the top-left corner; its
                // horizontal distance from that corner is the radius.
                let minx = sx0.min(sx1);
                let maxx = sx0.max(sx1);
                let miny = sy0.min(sy1);
                let maxy = sy0.max(sy1);
                let w = maxx - minx;
                let h = maxy - miny;
                let r = (cx - minx).clamp(0.0, (w * 0.5).min(h * 0.5));
                let mut data = self.clone();
                data.corner_radius = r;
                (data, offset)
            }
            _ => {
                // Box handles: move the touched edges, keep the opposite fixed.
                let mut minx = sx0.min(sx1);
                let mut miny = sy0.min(sy1);
                let mut maxx = sx0.max(sx1);
                let mut maxy = sy0.max(sy1);
                let (left, right, top, bottom) = match handle {
                    ShapeHandle::TopLeft => (true, false, true, false),
                    ShapeHandle::Top => (false, false, true, false),
                    ShapeHandle::TopRight => (false, true, true, false),
                    ShapeHandle::Right => (false, true, false, false),
                    ShapeHandle::BottomRight => (false, true, false, true),
                    ShapeHandle::Bottom => (false, false, false, true),
                    ShapeHandle::BottomLeft => (true, false, false, true),
                    ShapeHandle::Left => (true, false, false, false),
                    _ => (false, false, false, false),
                };
                if left {
                    minx = cx;
                }
                if right {
                    maxx = cx;
                }
                if top {
                    miny = cy;
                }
                if bottom {
                    maxy = cy;
                }
                self.rebuilt(minx, miny, maxx, maxy)
            }
        }
    }

    /// Render the shape into a tight RGBA buffer (straight alpha).
    ///
    /// Pixels are classified by the shape's signed distance: the AA ramp and
    /// the stroke ring get the exact per-pixel coverage math, while runs that
    /// are provably pure interior or pure exterior are slice-filled/skipped.
    /// All three SDFs are 1-Lipschitz (|sd(p)−sd(q)| ≤ |p−q|), so a sample
    /// `band+1+n` away from the boundary guarantees the next `n` pixels on the
    /// row stay outside the coverage band — page-sized shapes rasterize in
    /// O(perimeter + interior memset) instead of an SDF eval per pixel.
    pub fn render(&self) -> Option<ShapeRaster> {
        let (w, h) = self.raster_size();
        if w == 0 || h == 0 {
            return None;
        }
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        let fill_rgb = [
            self.fill_color[0] as f32 / 255.0,
            self.fill_color[1] as f32 / 255.0,
            self.fill_color[2] as f32 / 255.0,
        ];
        let fill_a = self.fill_color[3] as f32 / 255.0;
        let stroke_rgb = [
            self.stroke_color[0] as f32 / 255.0,
            self.stroke_color[1] as f32 / 255.0,
            self.stroke_color[2] as f32 / 255.0,
        ];
        let stroke_a = self.stroke_color[3] as f32 / 255.0;
        let half = self.stroke_width.max(0.0) * 0.5;
        // This geometry is constant for the whole raster; do not allocate it
        // again for every Polygon/Star pixel sample.
        let polygon_vertices = self.polygon_vertices();

        // Boundary-referenced signed distance per kind. A line is treated as a
        // "filled" band of half-width `half.max(0.5)` (matching coverage_parts,
        // where s = 0.5 − (d − half.max(0.5))), so one classifier serves all.
        let sd_at = |px: f32, py: f32| -> f32 {
            match self.kind {
                ShapeKind::Line => {
                    dist_to_segment(px, py, self.x0, self.y0, self.x1, self.y1) - half.max(0.5)
                }
                ShapeKind::Rectangle => {
                    let rx0 = self.x0.min(self.x1);
                    let rx1 = self.x0.max(self.x1);
                    let ry0 = self.y0.min(self.y1);
                    let ry1 = self.y0.max(self.y1);
                    if self.corner_radius > 0.0 {
                        let r = self
                            .corner_radius
                            .min((rx1 - rx0) * 0.5)
                            .min((ry1 - ry0) * 0.5)
                            .max(0.0);
                        sdf_round_box(px, py, rx0, ry0, rx1, ry1, r)
                    } else {
                        sdf_box(px, py, rx0, ry0, rx1, ry1)
                    }
                }
                ShapeKind::Ellipse => {
                    let cx = (self.x0 + self.x1) * 0.5;
                    let cy = (self.y0 + self.y1) * 0.5;
                    let a = (self.x1 - self.x0).abs() * 0.5;
                    let b = (self.y1 - self.y0).abs() * 0.5;
                    sdf_ellipse(px, py, cx, cy, a, b)
                }
                ShapeKind::Polygon | ShapeKind::Star => sdf_polygon(px, py, &polygon_vertices),
            }
        };
        // |sd| beyond this is provably coverage 0 (outside) or exactly the
        // interior colour (inside): fill_cov saturates at sd ≤ −0.5 and
        // stroke_cov vanishes at |sd| ≥ half + 0.5.
        let band = match self.kind {
            ShapeKind::Line => 0.5,
            _ => half + 0.5,
        };
        // Exact interior pixel, computed through the same blend as the
        // per-pixel path (fill_cov = 1, stroke_cov = 0; a line's interior is
        // its stroke at full coverage) so runs are byte-identical to it.
        let mut interior_px = [0u8; 4];
        match self.kind {
            ShapeKind::Line => blend_straight(&mut interior_px, stroke_rgb, stroke_a),
            _ => {
                if self.fill {
                    blend_straight(&mut interior_px, fill_rgb, fill_a);
                }
            }
        }

        for y in 0..h {
            let py = y as f32 + 0.5;
            let mut x: u32 = 0;
            while x < w {
                let px = x as f32 + 0.5;
                let sd = sd_at(px, py);
                if sd >= band + 1.0 {
                    // Pure exterior: skip the run the distance guarantees.
                    x += ((sd - band) as u32).max(1);
                } else if sd <= -(band + 1.0) {
                    // Pure interior: slice-fill the guaranteed run.
                    let run = (((-sd) - band) as u32).max(1).min(w - x);
                    if interior_px[3] > 0 {
                        let start = ((y * w + x) * 4) as usize;
                        for p in rgba[start..start + run as usize * 4].chunks_exact_mut(4) {
                            p.copy_from_slice(&interior_px);
                        }
                    }
                    x += run;
                } else {
                    // AA ramp / stroke ring: exact coverage.
                    // Reuse the signed distance already computed above. This
                    // avoids walking every polygon edge twice at boundary
                    // pixels (and keeps the Line band semantics unchanged).
                    let (fill_cov, stroke_cov) = match self.kind {
                        ShapeKind::Line => (0.0, (0.5 - sd).clamp(0.0, 1.0)),
                        _ => self.fill_stroke_parts(sd, half),
                    };
                    if fill_cov >= 0.0015 || stroke_cov >= 0.0015 {
                        let i = ((y * w + x) * 4) as usize;
                        let mut p = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
                        blend_straight(&mut p, fill_rgb, fill_cov * fill_a);
                        blend_straight(&mut p, stroke_rgb, stroke_cov * stroke_a);
                        rgba[i..i + 4].copy_from_slice(&p);
                    }
                    x += 1;
                }
            }
        }
        Some(ShapeRaster {
            rgba,
            width: w,
            height: h,
        })
    }

    /// Anti-aliased `(fill, stroke)` coverage of local pixel centre `(px,py)`.
    fn coverage_parts(&self, px: f32, py: f32) -> (f32, f32) {
        let half = self.stroke_width.max(0.0) * 0.5;
        match self.kind {
            ShapeKind::Line => {
                let d = dist_to_segment(px, py, self.x0, self.y0, self.x1, self.y1);
                let s = (0.5 - (d - half.max(0.5))).clamp(0.0, 1.0);
                (0.0, s)
            }
            ShapeKind::Rectangle => {
                let rx0 = self.x0.min(self.x1);
                let rx1 = self.x0.max(self.x1);
                let ry0 = self.y0.min(self.y1);
                let ry1 = self.y0.max(self.y1);
                let sd = if self.corner_radius > 0.0 {
                    let r = self
                        .corner_radius
                        .min((rx1 - rx0) * 0.5)
                        .min((ry1 - ry0) * 0.5)
                        .max(0.0);
                    sdf_round_box(px, py, rx0, ry0, rx1, ry1, r)
                } else {
                    sdf_box(px, py, rx0, ry0, rx1, ry1)
                };
                self.fill_stroke_parts(sd, half)
            }
            ShapeKind::Ellipse => {
                let cx = (self.x0 + self.x1) * 0.5;
                let cy = (self.y0 + self.y1) * 0.5;
                let a = (self.x1 - self.x0).abs() * 0.5;
                let b = (self.y1 - self.y0).abs() * 0.5;
                let sd = sdf_ellipse(px, py, cx, cy, a, b);
                self.fill_stroke_parts(sd, half)
            }
            ShapeKind::Polygon | ShapeKind::Star => {
                let sd = sdf_polygon(px, py, &self.polygon_vertices());
                self.fill_stroke_parts(sd, half)
            }
        }
    }

    fn fill_stroke_parts(&self, sd: f32, half: f32) -> (f32, f32) {
        let fill_cov = if self.fill {
            (0.5 - sd).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let stroke_cov = if half > 0.05 {
            (0.5 - (sd.abs() - half)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (fill_cov, stroke_cov)
    }
}

/// Snap raster-backed shape geometry onto pixel-friendly coordinates. Thin
/// strokes are crisp only when the path sits on a pixel centre for odd widths
/// and on a pixel edge for even widths; otherwise the SDF ramp necessarily splits
/// opacity across neighbouring pixels.
fn pixel_snapped_span(
    kind: ShapeKind,
    cx0: f32,
    cy0: f32,
    cx1: f32,
    cy1: f32,
    fill: bool,
    stroke_width: f32,
    stroke_color: [u8; 4],
) -> (f32, f32, f32, f32) {
    let visible_stroke = stroke_color[3] > 0
        && match kind {
            ShapeKind::Line => true,
            _ => stroke_width > 0.05,
        };
    let phase = if visible_stroke {
        stroke_pixel_phase(stroke_width)
    } else if fill {
        0.0
    } else {
        return (cx0, cy0, cx1, cy1);
    };

    match kind {
        ShapeKind::Line => {
            let dx = (cx1 - cx0).abs();
            let dy = (cy1 - cy0).abs();
            const AXIS_EPS: f32 = 0.001;
            if dy <= AXIS_EPS {
                let y = snap_to_phase((cy0 + cy1) * 0.5, phase);
                let (x0, x1) = snap_pair_to_phase(cx0, cx1, phase);
                (x0, y, x1, y)
            } else if dx <= AXIS_EPS {
                let x = snap_to_phase((cx0 + cx1) * 0.5, phase);
                let (y0, y1) = snap_pair_to_phase(cy0, cy1, phase);
                (x, y0, x, y1)
            } else {
                (cx0, cy0, cx1, cy1)
            }
        }
        // Rectangle / Ellipse / Polygon / Star: pixel-snap the bounding box.
        _ => {
            let (x0, x1) = snap_pair_to_phase(cx0, cx1, phase);
            let (y0, y1) = snap_pair_to_phase(cy0, cy1, phase);
            (x0, y0, x1, y1)
        }
    }
}

/// Signed distance to a closed polygon (negative inside), after Inigo Quilez's
/// `sdPoly`. `v` are the polygon vertices in order; fewer than 3 → +∞.
fn sdf_polygon(px: f32, py: f32, v: &[(f32, f32)]) -> f32 {
    let n = v.len();
    if n < 3 {
        return f32::INFINITY;
    }
    let mut d = {
        let dx = px - v[0].0;
        let dy = py - v[0].1;
        dx * dx + dy * dy
    };
    let mut s = 1.0f32;
    let mut j = n - 1;
    for i in 0..n {
        let e = (v[j].0 - v[i].0, v[j].1 - v[i].1);
        let w = (px - v[i].0, py - v[i].1);
        let denom = e.0 * e.0 + e.1 * e.1;
        let t = if denom > 0.0 {
            ((w.0 * e.0 + w.1 * e.1) / denom).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let bx = w.0 - e.0 * t;
        let by = w.1 - e.1 * t;
        d = d.min(bx * bx + by * by);
        let c = [py >= v[i].1, py < v[j].1, e.0 * w.1 > e.1 * w.0];
        if c[0] == c[1] && c[1] == c[2] {
            s = -s;
        }
        j = i;
    }
    s * d.sqrt()
}

fn stroke_pixel_phase(stroke_width: f32) -> f32 {
    let rounded = stroke_width.max(1.0).round() as i32;
    if rounded & 1 == 0 {
        0.0
    } else {
        0.5
    }
}

fn snap_to_phase(v: f32, phase: f32) -> f32 {
    (v - phase).round() + phase
}

fn snap_pair_to_phase(a: f32, b: f32, phase: f32) -> (f32, f32) {
    let sa = snap_to_phase(a, phase);
    let mut sb = snap_to_phase(b, phase);
    if (sb - sa).abs() < 1.0 && (b - a).abs() >= 0.5 {
        sb = sa + (b - a).signum();
    }
    (sa, sb)
}

/// Source-over blend of a straight-alpha colour into a `[r,g,b,a]` pixel.
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

/// Signed distance to an axis-aligned box (negative inside).
fn sdf_box(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let cx = (x0 + x1) * 0.5;
    let cy = (y0 + y1) * 0.5;
    let hx = (x1 - x0) * 0.5;
    let hy = (y1 - y0) * 0.5;
    let dx = (px - cx).abs() - hx;
    let dy = (py - cy).abs() - hy;
    let ox = dx.max(0.0);
    let oy = dy.max(0.0);
    (ox * ox + oy * oy).sqrt() + dx.max(dy).min(0.0)
}

/// Signed distance to a rounded box with corner radius `r` (negative inside).
fn sdf_round_box(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let cx = (x0 + x1) * 0.5;
    let cy = (y0 + y1) * 0.5;
    let hx = (x1 - x0) * 0.5 - r;
    let hy = (y1 - y0) * 0.5 - r;
    let dx = (px - cx).abs() - hx;
    let dy = (py - cy).abs() - hy;
    let ox = dx.max(0.0);
    let oy = dy.max(0.0);
    (ox * ox + oy * oy).sqrt() + dx.max(dy).min(0.0) - r
}

/// Approximate signed distance to an ellipse boundary (negative inside).
fn sdf_ellipse(px: f32, py: f32, cx: f32, cy: f32, a: f32, b: f32) -> f32 {
    let a = a.max(0.001);
    let b = b.max(0.001);
    let nx = (px - cx) / a;
    let ny = (py - cy) / b;
    let f = (nx * nx + ny * ny).sqrt();
    (f - 1.0) * a.min(b)
}

/// Distance from `(px,py)` to segment `(ax,ay)`→`(bx,by)`.
fn dist_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let dx = bx - ax;
    let dy = by - ay;
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-6 {
        let ex = px - ax;
        let ey = py - ay;
        return (ex * ex + ey * ey).sqrt();
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
    let cx = ax + t * dx;
    let cy = ay + t * dy;
    let ex = px - cx;
    let ey = py - cy;
    (ex * ex + ey * ey).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(kind: ShapeKind) -> ShapeData {
        let (d, _) = ShapeData::from_canvas_span(
            kind,
            10.0,
            20.0,
            60.0,
            80.0,
            0.0,
            true,
            [10, 20, 30, 255],
            2.0,
            [0, 0, 0, 255],
        );
        d
    }

    #[test]
    fn canvas_span_round_trips() {
        let (d, off) = ShapeData::from_canvas_span(
            ShapeKind::Rectangle,
            10.0,
            20.0,
            60.0,
            80.0,
            0.0,
            true,
            [1, 2, 3, 255],
            2.0,
            [0, 0, 0, 255],
        );
        let (a, b, c, e) = d.canvas_span(off);
        assert!((a - 10.0).abs() < 0.001);
        assert!((b - 20.0).abs() < 0.001);
        assert!((c - 60.0).abs() < 0.001);
        assert!((e - 80.0).abs() < 0.001);
    }

    /// The old rasterizer: exact per-pixel coverage over the whole bbox. The
    /// SDF skip-walk in `render` must stay byte-identical to it.
    fn render_reference(d: &ShapeData) -> Vec<u8> {
        let (w, h) = d.raster_size();
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        let fill_rgb = [
            d.fill_color[0] as f32 / 255.0,
            d.fill_color[1] as f32 / 255.0,
            d.fill_color[2] as f32 / 255.0,
        ];
        let fill_a = d.fill_color[3] as f32 / 255.0;
        let stroke_rgb = [
            d.stroke_color[0] as f32 / 255.0,
            d.stroke_color[1] as f32 / 255.0,
            d.stroke_color[2] as f32 / 255.0,
        ];
        let stroke_a = d.stroke_color[3] as f32 / 255.0;
        for y in 0..h {
            for x in 0..w {
                let (fill_cov, stroke_cov) = d.coverage_parts(x as f32 + 0.5, y as f32 + 0.5);
                if fill_cov < 0.0015 && stroke_cov < 0.0015 {
                    continue;
                }
                let i = ((y * w + x) * 4) as usize;
                let mut p = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
                blend_straight(&mut p, fill_rgb, fill_cov * fill_a);
                blend_straight(&mut p, stroke_rgb, stroke_cov * stroke_a);
                rgba[i..i + 4].copy_from_slice(&p);
            }
        }
        rgba
    }

    #[test]
    fn fast_render_matches_per_pixel_reference() {
        let cases = [
            // (kind, radius, fill, fill_color, stroke_w, stroke_color)
            (
                ShapeKind::Rectangle,
                0.0,
                true,
                [10, 20, 30, 255],
                2.0,
                [200, 40, 40, 255],
            ),
            (
                ShapeKind::Rectangle,
                18.0,
                true,
                [10, 20, 30, 200],
                6.0,
                [0, 0, 0, 255],
            ),
            (
                ShapeKind::Rectangle,
                12.0,
                false,
                [0, 0, 0, 0],
                3.0,
                [50, 90, 220, 255],
            ),
            (
                ShapeKind::Ellipse,
                0.0,
                true,
                [255, 128, 0, 128],
                5.0,
                [0, 0, 0, 180],
            ),
            (
                ShapeKind::Ellipse,
                0.0,
                true,
                [1, 2, 3, 255],
                0.0,
                [0, 0, 0, 0],
            ),
            (
                ShapeKind::Polygon,
                0.0,
                true,
                [40, 180, 90, 220],
                3.0,
                [20, 30, 40, 255],
            ),
            (
                ShapeKind::Star,
                0.0,
                true,
                [220, 160, 20, 180],
                4.0,
                [60, 20, 100, 255],
            ),
            (
                ShapeKind::Line,
                0.0,
                false,
                [0, 0, 0, 0],
                9.0,
                [30, 30, 30, 255],
            ),
            (
                ShapeKind::Line,
                0.0,
                false,
                [0, 0, 0, 0],
                1.0,
                [0, 0, 0, 255],
            ),
        ];
        for (kind, radius, fill, fc, sw, sc) in cases {
            let (d, _) =
                ShapeData::from_canvas_span(kind, 3.0, 7.0, 120.0, 90.0, radius, fill, fc, sw, sc);
            let fast = d.render().expect("raster").rgba;
            let reference = render_reference(&d);
            assert_eq!(
                fast, reference,
                "skip-walk must match per-pixel for {kind:?} r={radius} fill={fill} sw={sw}"
            );
        }
        // A diagonal line exercises the exterior skip on every row.
        let (d, _) = ShapeData::from_canvas_span(
            ShapeKind::Line,
            5.0,
            88.0,
            140.0,
            9.0,
            0.0,
            false,
            [0, 0, 0, 0],
            7.0,
            [80, 10, 90, 255],
        );
        assert_eq!(d.render().expect("raster").rgba, render_reference(&d));
    }

    #[test]
    fn render_fills_interior() {
        let d = base(ShapeKind::Rectangle);
        let r = d.render().expect("raster");
        // Centre of the shape (local ~ (pad+25, pad+30)) is opaque.
        let cx = ((d.x0 + d.x1) * 0.5) as u32;
        let cy = ((d.y0 + d.y1) * 0.5) as u32;
        let i = ((cy * r.width + cx) * 4) as usize;
        assert!(r.rgba[i + 3] > 250, "interior opaque");
        assert_eq!(&r.rgba[i..i + 3], &[10, 20, 30], "fill colour");
        // A corner of the raster is transparent (outside the shape + pad).
        assert_eq!(r.rgba[3], 0, "top-left padding transparent");
    }

    #[test]
    fn rectangle_snaps_fractional_span_for_odd_stroke() {
        let (d, off) = ShapeData::from_canvas_span(
            ShapeKind::Rectangle,
            10.2,
            20.2,
            60.2,
            80.2,
            0.0,
            false,
            [0, 0, 0, 0],
            1.0,
            [0, 0, 0, 255],
        );
        let (x0, y0, x1, y1) = d.canvas_span(off);
        for v in [x0, y0, x1, y1] {
            assert!(
                (v.fract() - 0.5).abs() < 0.001,
                "{v} should sit on pixel centre"
            );
        }
    }

    #[test]
    fn rectangle_snaps_fractional_span_for_even_stroke() {
        let (d, off) = ShapeData::from_canvas_span(
            ShapeKind::Rectangle,
            10.2,
            20.2,
            60.2,
            80.2,
            0.0,
            false,
            [0, 0, 0, 0],
            2.0,
            [0, 0, 0, 255],
        );
        let (x0, y0, x1, y1) = d.canvas_span(off);
        for v in [x0, y0, x1, y1] {
            assert!(v.fract().abs() < 0.001, "{v} should sit on pixel edge");
        }
    }

    #[test]
    fn odd_stroke_rectangle_renders_crisp_straight_edge() {
        let (d, _) = ShapeData::from_canvas_span(
            ShapeKind::Rectangle,
            10.2,
            20.2,
            60.2,
            80.2,
            0.0,
            false,
            [0, 0, 0, 0],
            1.0,
            [0, 0, 0, 255],
        );
        let r = d.render().expect("raster");
        let edge_x = d.x0.min(d.x1).floor() as u32;
        let y = ((d.y0 + d.y1) * 0.5).floor() as u32;
        let alpha_at = |x: u32| -> u8 {
            let i = ((y * r.width + x) * 4 + 3) as usize;
            r.rgba[i]
        };
        assert_eq!(alpha_at(edge_x), 255, "edge pixel should be fully opaque");
        assert_eq!(
            alpha_at(edge_x - 1),
            0,
            "outside neighbour should be transparent"
        );
        assert_eq!(
            alpha_at(edge_x + 1),
            0,
            "inside neighbour should not catch a soft stroke"
        );
    }

    #[test]
    fn resize_handle_moves_edge_keeps_opposite() {
        let d = base(ShapeKind::Rectangle);
        let off = {
            let (_, o) = ShapeData::from_canvas_span(
                ShapeKind::Rectangle,
                10.0,
                20.0,
                60.0,
                80.0,
                0.0,
                true,
                [10, 20, 30, 255],
                2.0,
                [0, 0, 0, 255],
            );
            o
        };
        let (d2, off2) = d.resize_by_handle(off, ShapeHandle::BottomRight, 100.0, 120.0);
        let (a, b, c, e) = d2.canvas_span(off2);
        assert!(
            (a - 10.0).abs() < 0.001 && (b - 20.0).abs() < 0.001,
            "top-left fixed"
        );
        assert!(
            (c - 100.0).abs() < 0.001 && (e - 120.0).abs() < 0.001,
            "corner followed"
        );
    }

    #[test]
    fn radius_handle_sets_corner_radius() {
        let (d, off) = ShapeData::from_canvas_span(
            ShapeKind::Rectangle,
            0.0,
            0.0,
            100.0,
            80.0,
            0.0,
            true,
            [10, 20, 30, 255],
            0.0,
            [0, 0, 0, 255],
        );
        let (d2, _) = d.resize_by_handle(off, ShapeHandle::Radius, 15.0, 0.0);
        assert!((d2.corner_radius - 15.0).abs() < 0.001);
        // Clamped to half the shorter side (40).
        let (d3, _) = d.resize_by_handle(off, ShapeHandle::Radius, 999.0, 0.0);
        assert!((d3.corner_radius - 40.0).abs() < 0.001);
    }
}
