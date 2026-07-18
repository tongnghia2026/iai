//! Warp forward-warp mesh.
//!
//! The warp is an accumulated *inverse* displacement field over a layer: for each
//! output pixel `o`, `disp(o)` is where that pixel was pushed FROM, so the warped
//! image is `out(o) = src(o - disp(o))`. Storing the inverse map (rather than a
//! forward scatter) makes rendering a simple per-output-pixel gather and lets many
//! brush dabs accumulate by summation — the same model GIMP's iwarp uses.
//!
//! The field lives on a coarse node grid (`cell` px spacing) because Warp warps
//! are low-frequency; the renderer bilinearly samples it. v1 brushes: Forward Warp
//! (push pixels along the drag) and Reconstruct (relax the field back toward zero).

use rayon::prelude::*;

/// Grid spacing in layer pixels. Warp warps are smooth, so a coarse mesh is
/// visually indistinguishable from per-pixel while keeping the field small.
pub const DEFAULT_CELL: usize = 4;

/// Active Warp brush. All modes drive the same displacement mesh; Freeze/Thaw
/// instead paint the protection mask that the warp modes respect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WarpMode {
    ForwardWarp,
    Reconstruct,
    Pucker,
    Bloat,
    Twirl,
    PushLeft,
    Freeze,
    Thaw,
}

impl WarpMode {
    pub fn label(self) -> &'static str {
        match self {
            WarpMode::ForwardWarp => "Forward Warp",
            WarpMode::Reconstruct => "Reconstruct",
            WarpMode::Pucker => "Pucker",
            WarpMode::Bloat => "Bloat",
            WarpMode::Twirl => "Twirl",
            WarpMode::PushLeft => "Push Left",
            WarpMode::Freeze => "Freeze",
            WarpMode::Thaw => "Thaw",
        }
    }

    pub const ALL: [WarpMode; 8] = [
        WarpMode::ForwardWarp,
        WarpMode::Reconstruct,
        WarpMode::Pucker,
        WarpMode::Bloat,
        WarpMode::Twirl,
        WarpMode::PushLeft,
        WarpMode::Freeze,
        WarpMode::Thaw,
    ];

    /// True for brushes that keep acting while the pointer is held even without
    /// movement (radial / rotational / mask brushes), so a press applies a dab.
    pub fn acts_on_press(self) -> bool {
        !matches!(self, WarpMode::ForwardWarp | WarpMode::PushLeft)
    }

    pub fn is_mask(self) -> bool {
        matches!(self, WarpMode::Freeze | WarpMode::Thaw)
    }
}

/// Brush parameters edited from the Warp panel.
#[derive(Clone, Copy, Debug)]
pub struct WarpParams {
    pub mode: WarpMode,
    /// Brush diameter in layer pixels.
    pub size: f32,
    /// 0..1 — scales how much each dab moves (Forward Warp) or relaxes (Reconstruct).
    pub pressure: f32,
}

impl Default for WarpParams {
    fn default() -> Self {
        Self {
            mode: WarpMode::ForwardWarp,
            size: 200.0,
            pressure: 0.5,
        }
    }
}

#[derive(Clone)]
pub struct WarpMesh {
    pub layer_w: usize,
    pub layer_h: usize,
    pub cell: usize,
    /// Node grid dimensions: `layer_w/cell + 1` so the grid spans `[0, layer_w]`
    /// inclusive (the far edge is a real node, not extrapolated).
    pub gw: usize,
    pub gh: usize,
    /// Accumulated inverse displacement per node, in layer pixels.
    pub dx: Vec<f32>,
    pub dy: Vec<f32>,
    /// Per-node freeze weight (0 = movable, 1 = locked). Warp deltas are scaled by
    /// `1 - frozen` so painted-frozen regions resist the brush.
    pub frozen: Vec<f32>,
    /// False while the field is all-zero (identity) — lets callers skip work.
    pub touched: bool,
    /// True once any node has a non-zero freeze weight (drives the mask overlay).
    pub any_frozen: bool,
}

/// Per-dab gains so radial / rotational brushes accumulate gently (one dab moves a
/// fraction of the distance-to-centre at the peak; many dabs build up).
const RADIAL_STEP: f32 = 0.16;
const TWIRL_STEP: f32 = 0.14;

impl WarpMesh {
    pub fn new(layer_w: usize, layer_h: usize, cell: usize) -> Self {
        let cell = cell.max(1);
        let gw = layer_w.div_ceil(cell) + 1;
        let gh = layer_h.div_ceil(cell) + 1;
        Self {
            layer_w,
            layer_h,
            cell,
            gw,
            gh,
            dx: vec![0.0; gw * gh],
            dy: vec![0.0; gw * gh],
            frozen: vec![0.0; gw * gh],
            touched: false,
            any_frozen: false,
        }
    }

    /// Reset the displacement field to identity (Restore All). Keeps the freeze mask.
    pub fn clear(&mut self) {
        self.dx.iter_mut().for_each(|v| *v = 0.0);
        self.dy.iter_mut().for_each(|v| *v = 0.0);
        self.touched = false;
    }

    /// Bilinear-sample the inverse displacement at layer pixel `(x, y)`.
    pub fn sample(&self, x: f32, y: f32) -> (f32, f32) {
        let cell = self.cell as f32;
        let gx = (x / cell).clamp(0.0, (self.gw - 1) as f32);
        let gy = (y / cell).clamp(0.0, (self.gh - 1) as f32);
        let x0 = gx.floor() as usize;
        let y0 = gy.floor() as usize;
        let x1 = (x0 + 1).min(self.gw - 1);
        let y1 = (y0 + 1).min(self.gh - 1);
        let wx = gx - x0 as f32;
        let wy = gy - y0 as f32;
        let i00 = y0 * self.gw + x0;
        let i10 = y0 * self.gw + x1;
        let i01 = y1 * self.gw + x0;
        let i11 = y1 * self.gw + x1;
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let dx = lerp(
            lerp(self.dx[i00], self.dx[i10], wx),
            lerp(self.dx[i01], self.dx[i11], wx),
            wy,
        );
        let dy = lerp(
            lerp(self.dy[i00], self.dy[i10], wx),
            lerp(self.dy[i01], self.dy[i11], wx),
            wy,
        );
        (dx, dy)
    }

    /// The grid-node index range whose nodes fall within `radius` of `(cx, cy)`.
    fn node_span(&self, cx: f32, cy: f32, radius: f32) -> (usize, usize, usize, usize) {
        let cell = self.cell as f32;
        let gx0 = (((cx - radius) / cell).floor() as isize).max(0) as usize;
        let gy0 = (((cy - radius) / cell).floor() as isize).max(0) as usize;
        let gx1 = (((cx + radius) / cell).ceil() as isize).clamp(0, self.gw as isize - 1) as usize;
        let gy1 = (((cy + radius) / cell).ceil() as isize).clamp(0, self.gh as isize - 1) as usize;
        (gx0, gy0, gx1.max(gx0), gy1.max(gy0))
    }

    /// Smooth radial falloff: 1 at the brush centre, 0 at the edge, with zero slope
    /// at both ends so repeated dabs blend without a hard rim.
    #[inline]
    fn falloff(d: f32, radius: f32) -> f32 {
        if radius <= 0.0 {
            return 0.0;
        }
        let t = (d / radius).clamp(0.0, 1.0);
        let s = t * t * (3.0 - 2.0 * t); // smoothstep 0→1
        1.0 - s
    }

    /// Visit each grid node within `radius` of `(cx, cy)`, passing its index, its
    /// `(nx, ny)` layer-pixel position, and its freeze-scaled falloff weight. The
    /// freeze scaling (`1 - frozen`) makes every warp brush respect the mask.
    #[inline]
    fn for_each_node(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        mut f: impl FnMut(&mut Self, usize, f32, f32, f32),
    ) {
        if radius <= 0.0 {
            return;
        }
        let (gx0, gy0, gx1, gy1) = self.node_span(cx, cy, radius);
        let cell = self.cell as f32;
        for gy in gy0..=gy1 {
            let ny = gy as f32 * cell;
            for gx in gx0..=gx1 {
                let nx = gx as f32 * cell;
                let d = ((nx - cx).powi(2) + (ny - cy).powi(2)).sqrt();
                let fall = Self::falloff(d, radius);
                if fall <= 0.0 {
                    continue;
                }
                let i = gy * self.gw + gx;
                let w = fall * (1.0 - self.frozen[i]);
                if w > 0.0 {
                    f(self, i, nx, ny, w);
                }
            }
        }
    }

    /// Forward Warp: push pixels under the brush along `(mvx, mvy)` (the pointer
    /// delta this dab, in layer px). `strength` folds in pressure/density (0..1).
    pub fn forward_warp(
        &mut self,
        cx: f32,
        cy: f32,
        mvx: f32,
        mvy: f32,
        radius: f32,
        strength: f32,
    ) {
        if (mvx == 0.0 && mvy == 0.0) || strength <= 0.0 {
            return;
        }
        self.for_each_node(cx, cy, radius, |m, i, _nx, _ny, w| {
            m.dx[i] += mvx * w * strength;
            m.dy[i] += mvy * w * strength;
        });
        self.touched = true;
    }

    /// Push Left: move pixels perpendicular to the drag (to its left), the classic
    /// "smear sideways" brush. `strength` folds in pressure (0..1).
    pub fn push_left(&mut self, cx: f32, cy: f32, mvx: f32, mvy: f32, radius: f32, strength: f32) {
        if (mvx == 0.0 && mvy == 0.0) || strength <= 0.0 {
            return;
        }
        // 90° CCW of the drag = its left side.
        let (px, py) = (-mvy, mvx);
        self.for_each_node(cx, cy, radius, |m, i, _nx, _ny, w| {
            m.dx[i] += px * w * strength;
            m.dy[i] += py * w * strength;
        });
        self.touched = true;
    }

    /// Pucker (shrink): pull pixels toward the brush centre. `strength` 0..1.
    pub fn pucker(&mut self, cx: f32, cy: f32, radius: f32, strength: f32) {
        if strength <= 0.0 {
            return;
        }
        let k = strength * RADIAL_STEP;
        self.for_each_node(cx, cy, radius, |m, i, nx, ny, w| {
            m.dx[i] += (cx - nx) * w * k;
            m.dy[i] += (cy - ny) * w * k;
        });
        self.touched = true;
    }

    /// Bloat (grow): push pixels away from the brush centre. `strength` 0..1.
    pub fn bloat(&mut self, cx: f32, cy: f32, radius: f32, strength: f32) {
        if strength <= 0.0 {
            return;
        }
        let k = strength * RADIAL_STEP;
        self.for_each_node(cx, cy, radius, |m, i, nx, ny, w| {
            m.dx[i] += (nx - cx) * w * k;
            m.dy[i] += (ny - cy) * w * k;
        });
        self.touched = true;
    }

    /// Twirl: rotate pixels around the brush centre. `strength` 0..1; positive turns
    /// clockwise (negative for counter-clockwise).
    pub fn twirl(&mut self, cx: f32, cy: f32, radius: f32, strength: f32) {
        if strength == 0.0 {
            return;
        }
        let k = strength * TWIRL_STEP;
        self.for_each_node(cx, cy, radius, |m, i, nx, ny, w| {
            let (rx, ry) = (nx - cx, ny - cy);
            // 90° clockwise rotation of (rx, ry).
            m.dx[i] += ry * w * k;
            m.dy[i] += -rx * w * k;
        });
        self.touched = true;
    }

    /// Reconstruct: relax the field toward identity under the brush. `amount` (0..1)
    /// is how far each affected node moves back to zero this dab.
    pub fn reconstruct(&mut self, cx: f32, cy: f32, radius: f32, amount: f32) {
        if amount <= 0.0 {
            return;
        }
        self.for_each_node(cx, cy, radius, |m, i, _nx, _ny, w| {
            let kk = (1.0 - w * amount).clamp(0.0, 1.0);
            m.dx[i] *= kk;
            m.dy[i] *= kk;
        });
    }

    /// Freeze (`thaw=false`) or Thaw (`thaw=true`) the protection mask under the
    /// brush. `amount` 0..1 is the per-dab change; the mask drives both the warp
    /// scaling and the red overlay.
    pub fn paint_freeze(&mut self, cx: f32, cy: f32, radius: f32, amount: f32, thaw: bool) {
        if radius <= 0.0 || amount <= 0.0 {
            return;
        }
        let (gx0, gy0, gx1, gy1) = self.node_span(cx, cy, radius);
        let cell = self.cell as f32;
        for gy in gy0..=gy1 {
            let ny = gy as f32 * cell;
            for gx in gx0..=gx1 {
                let nx = gx as f32 * cell;
                let d = ((nx - cx).powi(2) + (ny - cy).powi(2)).sqrt();
                let w = Self::falloff(d, radius) * amount;
                if w > 0.0 {
                    let i = gy * self.gw + gx;
                    self.frozen[i] = if thaw {
                        (self.frozen[i] - w).max(0.0)
                    } else {
                        (self.frozen[i] + w).min(1.0)
                    };
                }
            }
        }
        self.any_frozen = self.frozen.iter().any(|&f| f > 0.001);
    }

    /// Downsampled freeze mask as an alpha grid (`gw × gh`, 0..255) for the overlay.
    /// Returns `None` when nothing is frozen.
    pub fn freeze_alpha(&self) -> Option<(usize, usize, Vec<u8>)> {
        if !self.any_frozen {
            return None;
        }
        let data = self
            .frozen
            .iter()
            .map(|&f| (f.clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect();
        Some((self.gw, self.gh, data))
    }

    /// Output rect (clamped, half-open) affected by a dab at `(cx, cy)` — the brush
    /// bounding box, since `falloff` is zero outside `radius`. Returned as
    /// `(x0, y0, w, h)` for the incremental re-render.
    pub fn dab_rect(&self, cx: f32, cy: f32, radius: f32) -> (u32, u32, u32, u32) {
        let x0 = ((cx - radius).floor() as isize).max(0) as u32;
        let y0 = ((cy - radius).floor() as isize).max(0) as u32;
        let x1 = (((cx + radius).ceil() as isize).clamp(0, self.layer_w as isize)) as u32;
        let y1 = (((cy + radius).ceil() as isize).clamp(0, self.layer_h as isize)) as u32;
        (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
    }

    /// Re-render the `(rx, ry, rw, rh)` rect of `out` (a full `layer_w*layer_h`
    /// RGBA buffer) by gathering from `src` (the untouched original, same size) at
    /// `o - disp(o)`, bilinear with edge clamp. Rows run in parallel.
    pub fn warp_region_into(&self, src: &[u8], out: &mut [u8], rx: u32, ry: u32, rw: u32, rh: u32) {
        if rw == 0 || rh == 0 {
            return;
        }
        let w = self.layer_w;
        let h = self.layer_h;
        let stride = w * 4;
        let rx = rx as usize;
        let ry = ry as usize;
        let rw = (rw as usize).min(w.saturating_sub(rx));
        let rh = (rh as usize).min(h.saturating_sub(ry));

        out[ry * stride..(ry + rh) * stride]
            .par_chunks_mut(stride)
            .enumerate()
            .for_each(|(row, chunk)| {
                let y = ry + row;
                for px in rx..rx + rw {
                    let (dx, dy) = self.sample(px as f32, y as f32);
                    let sx = px as f32 - dx;
                    let sy = y as f32 - dy;
                    let rgba = sample_bilinear(src, w, h, sx, sy);
                    let o = px * 4;
                    chunk[o] = rgba[0];
                    chunk[o + 1] = rgba[1];
                    chunk[o + 2] = rgba[2];
                    chunk[o + 3] = rgba[3];
                }
            });
    }
}

/// Bilinear RGBA sample with edge clamp. Straight (non-premultiplied) — adequate
/// for opaque photo layers; premultiply is a future refinement for hard alpha
/// edges.
fn sample_bilinear(src: &[u8], w: usize, h: usize, x: f32, y: f32) -> [u8; 4] {
    if w == 0 || h == 0 {
        return [0, 0, 0, 0];
    }
    let fx = x.clamp(0.0, (w - 1) as f32);
    let fy = y.clamp(0.0, (h - 1) as f32);
    let x0 = fx.floor() as usize;
    let y0 = fy.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let wx = fx - x0 as f32;
    let wy = fy - y0 as f32;
    let p = |px: usize, py: usize, c: usize| src[(py * w + px) * 4 + c] as f32;
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = p(x0, y0, c) + (p(x1, y0, c) - p(x0, y0, c)) * wx;
        let bot = p(x0, y1, c) + (p(x1, y1, c) - p(x0, y1, c)) * wx;
        out[c] = (top + (bot - top) * wy).round().clamp(0.0, 255.0) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let p = f(x, y);
                let o = (y * w + x) * 4;
                v[o..o + 4].copy_from_slice(&p);
            }
        }
        v
    }

    #[test]
    fn identity_mesh_reproduces_source() {
        let (w, h) = (32usize, 24usize);
        let src = solid(w, h, |x, y| [(x * 7) as u8, (y * 9) as u8, 0, 255]);
        let mesh = WarpMesh::new(w, h, DEFAULT_CELL);
        let mut out = vec![0u8; w * h * 4];
        mesh.warp_region_into(&src, &mut out, 0, 0, w as u32, h as u32);
        assert_eq!(out, src, "identity field must be a no-op");
    }

    #[test]
    fn forward_warp_moves_pixels_along_drag() {
        // A vertical edge at x=16: left red, right blue. Push rightward at the edge
        // and the red/blue boundary should shift right (red invades blue).
        let (w, h) = (64usize, 16usize);
        let src = solid(w, h, |x, _| {
            if x < 32 {
                [200, 0, 0, 255]
            } else {
                [0, 0, 200, 255]
            }
        });
        let mut mesh = WarpMesh::new(w, h, 2);
        // Drag +8px in x, centred on the edge, big radius.
        mesh.forward_warp(32.0, 8.0, 8.0, 0.0, 20.0, 1.0);
        let mut out = src.clone();
        let (rx, ry, rw, rh) = mesh.dab_rect(32.0, 8.0, 20.0);
        mesh.warp_region_into(&src, &mut out, rx, ry, rw, rh);
        // At the centre row, a pixel just right of the old edge should now be red-ish
        // (it sampled from the left side after the inverse displacement).
        let idx = (8 * w + 36) * 4;
        assert!(
            out[idx] > out[idx + 2],
            "expected red>blue after rightward push, got {:?}",
            &out[idx..idx + 4]
        );
    }

    #[test]
    fn reconstruct_relaxes_field() {
        let mut mesh = WarpMesh::new(40, 40, 4);
        mesh.forward_warp(20.0, 20.0, 6.0, 0.0, 15.0, 1.0);
        let (before_dx, _) = mesh.sample(20.0, 20.0);
        assert!(before_dx.abs() > 0.1);
        for _ in 0..40 {
            mesh.reconstruct(20.0, 20.0, 15.0, 0.5);
        }
        let (after_dx, _) = mesh.sample(20.0, 20.0);
        assert!(
            after_dx.abs() < before_dx.abs() * 0.1,
            "reconstruct should relax the field toward zero ({before_dx} -> {after_dx})"
        );
    }
}
