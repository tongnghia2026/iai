//! Reshaping by moved control points. Each moved point (and each still one)
//! steers a smooth inverse displacement field: where every output pixel takes
//! its colour from. The field is moving least squares with rigid transforms
//! (Schaefer et al. 2006), which bends straight lines between the points as
//! little as possible, evaluated on a coarse grid over the area of influence
//! and interpolated. A ring of still anchors holds the picture around the
//! moved points; past it the field fades to nothing, so the background away
//! from a face never moves.

use rayon::prelude::*;

use super::analysis::{Clip, PortraitModel};
use super::geometry::{
    loop_points, Region, FACE_OVAL, LEFT_BROW, LEFT_EYE, LEFT_IRIS, LIPS_OUTER, NOSE_BRIDGE,
    RIGHT_BROW, RIGHT_EYE, RIGHT_IRIS,
};

/// A control point the field carries from `from` to `to` (image pixels);
/// still points have both the same.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Control {
    pub from: [f32; 2],
    pub to: [f32; 2],
}

impl Control {
    pub fn still(at: [f32; 2]) -> Self {
        Self { from: at, to: at }
    }

    fn moved(&self) -> bool {
        self.from != self.to
    }
}

/// Inverse displacement over `region`: output pixel `o` takes its colour from
/// `o + offset(o)`. Zero outside the region and fading to zero at its edge.
#[derive(Clone, Debug)]
pub struct Displacement {
    pub region: Region,
    cell: f32,
    gw: usize,
    gh: usize,
    offset: Vec<[f32; 2]>,
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Where rigid moving least squares carries `v`, given control points `p`
/// carried to `q`.
fn mls_rigid(v: [f32; 2], p: &[[f32; 2]], q: &[[f32; 2]]) -> [f32; 2] {
    let weight = |i: usize| {
        let (dx, dy) = (p[i][0] - v[0], p[i][1] - v[1]);
        dx * dx + dy * dy
    };
    let (mut total, mut ps, mut qs) = (0.0f32, [0.0f32; 2], [0.0f32; 2]);
    for i in 0..p.len() {
        let d2 = weight(i);
        if d2 < 1e-6 {
            return q[i];
        }
        let w = 1.0 / d2;
        total += w;
        for k in 0..2 {
            ps[k] += w * p[i][k];
            qs[k] += w * q[i][k];
        }
    }
    if total == 0.0 {
        return v;
    }
    let (ps, qs) = (ps.map(|s| s / total), qs.map(|s| s / total));
    let d = [v[0] - ps[0], v[1] - ps[1]];
    let mut f = [0.0f32; 2];
    for i in 0..p.len() {
        let w = 1.0 / weight(i);
        let (a, b) = (p[i][0] - ps[0], p[i][1] - ps[1]);
        let (u, t) = (q[i][0] - qs[0], q[i][1] - qs[1]);
        f[0] += w * (u * (a * d[0] + b * d[1]) + t * (b * d[0] - a * d[1]));
        f[1] += w * (u * (a * d[1] - b * d[0]) + t * (b * d[1] + a * d[0]));
    }
    let (length, reach) = (f[0].hypot(f[1]), d[0].hypot(d[1]));
    if length < 1e-12 {
        return [qs[0] + d[0], qs[1] + d[1]];
    }
    [qs[0] + f[0] / length * reach, qs[1] + f[1] / length * reach]
}

impl Displacement {
    /// The field over `region` (clipped to the `width` x `height` image) for
    /// `controls`, evaluated every `cell` pixels; it fades to zero over the
    /// last `fade` pixels inside the region's edges. `None` when nothing
    /// moves.
    pub fn from_controls(
        controls: &[Control],
        region: Region,
        cell: f32,
        fade: f32,
    ) -> Option<Self> {
        if region.is_empty() || !controls.iter().any(Control::moved) {
            return None;
        }
        let cell = cell.max(1.0);
        let gw = (region.w as f32 / cell).ceil() as usize + 1;
        let gh = (region.h as f32 / cell).ceil() as usize + 1;
        // Inverse: from where each point lands back to where it came from.
        let p: Vec<[f32; 2]> = controls.iter().map(|c| c.to).collect();
        let q: Vec<[f32; 2]> = controls.iter().map(|c| c.from).collect();
        let (x1, y1) = ((region.x + region.w) as f32, (region.y + region.h) as f32);
        let offset = (0..gw * gh)
            .into_par_iter()
            .map(|n| {
                let x = region.x as f32 + (n % gw) as f32 * cell;
                let y = region.y as f32 + (n / gw) as f32 * cell;
                let edge = (x - region.x as f32)
                    .min(y - region.y as f32)
                    .min(x1 - x)
                    .min(y1 - y);
                let keep = smoothstep(0.0, fade.max(1.0), edge);
                if keep == 0.0 {
                    return [0.0; 2];
                }
                let s = mls_rigid([x, y], &p, &q);
                [(s[0] - x) * keep, (s[1] - y) * keep]
            })
            .collect();
        Some(Self {
            region,
            cell,
            gw,
            gh,
            offset,
        })
    }

    /// Offset at image point (x, y), bilinear between grid nodes.
    pub fn offset_at(&self, x: f32, y: f32) -> [f32; 2] {
        let r = self.region;
        let (u, v) = ((x - r.x as f32) / self.cell, (y - r.y as f32) / self.cell);
        if u < 0.0 || v < 0.0 || u > (self.gw - 1) as f32 || v > (self.gh - 1) as f32 {
            return [0.0; 2];
        }
        let (i, j) = ((u as usize).min(self.gw - 2), (v as usize).min(self.gh - 2));
        let (fu, fv) = (u - i as f32, v - j as f32);
        let at = |a: usize, b: usize| self.offset[b * self.gw + a];
        let (o00, o10, o01, o11) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
        std::array::from_fn(|k| {
            let top = o00[k] + (o10[k] - o00[k]) * fu;
            let bottom = o01[k] + (o11[k] - o01[k]) * fu;
            top + (bottom - top) * fv
        })
    }

    /// The largest offset anywhere, in pixels.
    pub fn largest(&self) -> f32 {
        self.offset
            .iter()
            .map(|o| o[0].hypot(o[1]))
            .fold(0.0, f32::max)
    }
}

/// `src` (the RGBA pixels of `source`, a region of the image) warped by the
/// sum of `fields` over `region`, as that region's RGBA pixels; colour is
/// taken from within `source`. Within a selection (`clip`) the offsets are
/// scaled by its coverage, so the warp eases out along its edge instead of
/// blending two images.
pub fn warp_region(
    src: &[u8],
    source: Region,
    fields: &[&Displacement],
    clip: Option<&Clip>,
    region: Region,
) -> Vec<u8> {
    let (rw, w, h) = (region.w as usize, source.w as usize, source.h as usize);
    let (sx, sy) = (source.x as f32, source.y as f32);
    let mut out = vec![0u8; region.len() * 4];
    out.par_chunks_mut(rw * 4)
        .enumerate()
        .for_each(|(row, line)| {
            let y = region.y + row as u32;
            for col in 0..rw {
                let x = region.x + col as u32;
                let (fx, fy) = (x as f32, y as f32);
                let mut offset = [0.0f32; 2];
                for field in fields {
                    let o = field.offset_at(fx, fy);
                    offset[0] += o[0];
                    offset[1] += o[1];
                }
                if let Some(clip) = clip {
                    let a = clip.at(x, y);
                    offset = offset.map(|v| v * a);
                }
                let (u, v) = (fx + offset[0] - sx, fy + offset[1] - sy);
                // Still pixels copy straight across.
                let px = if offset[0].abs() < 0.01 && offset[1].abs() < 0.01 {
                    let (u, v) = (
                        (u.round() as usize).min(w - 1),
                        (v.round() as usize).min(h - 1),
                    );
                    let o = (v * w + u) * 4;
                    [src[o], src[o + 1], src[o + 2], src[o + 3]]
                } else {
                    sample(src, w, h, u, v)
                };
                line[col * 4..col * 4 + 4].copy_from_slice(&px);
            }
        });
    out
}

/// Bilinear RGBA sample at (x, y), clamped to the image.
fn sample(rgba: &[u8], w: usize, h: usize, x: f32, y: f32) -> [u8; 4] {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let p = |px: usize, py: usize, k: usize| rgba[(py * w + px) * 4 + k] as f32;
    std::array::from_fn(|k| {
        let top = p(x0, y0, k) + (p(x1, y0, k) - p(x0, y0, k)) * fx;
        let bottom = p(x0, y1, k) + (p(x1, y1, k) - p(x0, y1, k)) * fx;
        (top + (bottom - top) * fy).round() as u8
    })
}

/// Face shape sliders, -100..100 (0 = as shot).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FaceShape {
    /// Draw the jaw and cheeks in (positive) or out.
    pub slim: f32,
    /// Lengthen (positive) or shorten the chin.
    pub chin: f32,
    /// Enlarge (positive) or narrow the eyes.
    pub eyes: f32,
    /// Narrow (positive) or widen the nose wings.
    pub nose: f32,
    /// Widen (positive) or narrow the mouth.
    pub mouth: f32,
    /// Raise (positive) or lower the forehead's top.
    pub forehead: f32,
}

impl FaceShape {
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }
}

/// Outer edges of the nostril wings, and the wing creases above them.
const NOSE_WINGS: [u16; 10] = [98, 327, 64, 294, 48, 278, 129, 358, 102, 331];

/// A face's frame: centre, extent (forehead to chin), and the unit vectors
/// across (subject's right to left) and down it.
struct Frame {
    centre: [f32; 2],
    extent: f32,
    across: [f32; 2],
    down: [f32; 2],
}

impl Frame {
    fn new(points: &[[f32; 3]]) -> Self {
        let pick = |k: usize| [points[k][0], points[k][1]];
        let (right, left) = (pick(RIGHT_IRIS.0 as usize), pick(LEFT_IRIS.0 as usize));
        let angle = (left[1] - right[1]).atan2(left[0] - right[0]);
        let (sin, cos) = angle.sin_cos();
        let (mut x0, mut x1, mut y0, mut y1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in points {
            x0 = x0.min(p[0]);
            x1 = x1.max(p[0]);
            y0 = y0.min(p[1]);
            y1 = y1.max(p[1]);
        }
        let top = pick(10);
        let chin = pick(152);
        Self {
            centre: [(x0 + x1) * 0.5, (y0 + y1) * 0.5],
            extent: (chin[0] - top[0]).hypot(chin[1] - top[1]).max(1.0),
            across: [cos, sin],
            down: [-sin, cos],
        }
    }

    /// (across, down) coordinates of `p` relative to `origin`.
    fn local(&self, p: [f32; 2], origin: [f32; 2]) -> [f32; 2] {
        let (dx, dy) = (p[0] - origin[0], p[1] - origin[1]);
        [
            dx * self.across[0] + dy * self.across[1],
            dx * self.down[0] + dy * self.down[1],
        ]
    }

    fn shift(&self, p: [f32; 2], across: f32, down: f32) -> [f32; 2] {
        [
            p[0] + self.across[0] * across + self.down[0] * down,
            p[1] + self.across[1] * across + self.down[1] * down,
        ]
    }
}

/// Control points of one face (its 478 mesh `points`) for `shape`, with
/// the region and fade the field needs: the moved outline and features, the
/// features left still, and a ring of anchors around the face. `others` are
/// points of other faces, held still where they fall near this one.
/// `None` when the shape is neutral.
pub fn face_controls(
    points: &[[f32; 3]],
    shape: &FaceShape,
    others: &[[f32; 2]],
    width: u32,
    height: u32,
) -> Option<(Vec<Control>, Region, f32)> {
    if shape.is_neutral() || points.len() < 478 {
        return None;
    }
    let f = Frame::new(points);
    let e = f.extent;
    let pick = |k: u16| [points[k as usize][0], points[k as usize][1]];
    let unit = |v: f32| v.clamp(-100.0, 100.0) / 100.0;
    // The face's midline: through the nose bridge and the chin.
    let mid_top = pick(168);
    let eye_level = f.local(
        [
            (pick(RIGHT_IRIS.0)[0] + pick(LEFT_IRIS.0)[0]) * 0.5,
            (pick(RIGHT_IRIS.0)[1] + pick(LEFT_IRIS.0)[1]) * 0.5,
        ],
        mid_top,
    )[1];
    let chin_level = f.local(pick(152), mid_top)[1];
    let span = (chin_level - eye_level).max(1.0);
    // Down the face from the eye line (0) to the chin (1).
    let height_of = |p: [f32; 2]| (f.local(p, mid_top)[1] - eye_level) / span;
    let across_of = |p: [f32; 2]| f.local(p, mid_top)[0];

    let mut moves: std::collections::HashMap<u16, [f32; 2]> = Default::default();
    let mut nudge = |k: u16, across: f32, down: f32| {
        let m = moves.entry(k).or_insert([0.0; 2]);
        m[0] += across;
        m[1] += down;
    };

    // Jaw and cheeks: toward the midline, most at the jaw's angle, little
    // at the temples, less toward the chin.
    let slim = unit(shape.slim);
    if slim != 0.0 {
        for &k in &FACE_OVAL {
            let p = pick(k);
            let t = height_of(p);
            let profile = smoothstep(0.05, 0.55, t) * (1.0 - 0.6 * smoothstep(0.8, 1.0, t));
            nudge(k, -across_of(p) * 0.09 * slim * profile, 0.0);
        }
    }
    // Chin: down (longer) or up, most at its point.
    let chin = unit(shape.chin);
    if chin != 0.0 {
        let half = across_of(pick(397))
            .abs()
            .max(across_of(pick(172)).abs())
            .max(1.0);
        for &k in &FACE_OVAL {
            let p = pick(k);
            let t = height_of(p);
            let side = 1.0 - smoothstep(0.1, 0.8, across_of(p).abs() / half);
            nudge(k, 0.0, 0.05 * e * chin * smoothstep(0.7, 0.95, t) * side);
        }
    }
    // Forehead top: up or down, most at its middle.
    let forehead = unit(shape.forehead);
    if forehead != 0.0 {
        let half = across_of(pick(234))
            .abs()
            .max(across_of(pick(454)).abs())
            .max(1.0);
        for &k in &FACE_OVAL {
            let p = pick(k);
            let t = height_of(p);
            let side = 1.0 - smoothstep(0.3, 1.0, across_of(p).abs() / half);
            nudge(
                k,
                0.0,
                -0.06 * e * forehead * smoothstep(-0.3, -0.7, t) * side,
            );
        }
    }
    // Eyes: scaled about their irises.
    let eyes = unit(shape.eyes);
    if eyes != 0.0 {
        for (eye, iris) in [(&RIGHT_EYE, RIGHT_IRIS), (&LEFT_EYE, LEFT_IRIS)] {
            let c = pick(iris.0);
            let scale = 0.14 * eyes;
            for &k in eye.iter().chain(&iris.1) {
                let p = pick(k);
                let d = f.local(p, c);
                nudge(k, d[0] * scale, d[1] * scale);
            }
        }
    }
    // Nose wings: toward the nose's midline.
    let nose = unit(shape.nose);
    if nose != 0.0 {
        let tip = pick(4);
        for &k in &NOSE_WINGS {
            let d = f.local(pick(k), tip)[0];
            nudge(k, -d * 0.15 * nose, 0.0);
        }
    }
    // Mouth corners: out (wider) or in.
    let mouth = unit(shape.mouth);
    if mouth != 0.0 {
        let centre = [
            (pick(13)[0] + pick(14)[0]) * 0.5,
            (pick(13)[1] + pick(14)[1]) * 0.5,
        ];
        let half = (f.local(pick(291), centre)[0] - f.local(pick(61), centre)[0]).abs() * 0.5;
        for &k in &LIPS_OUTER {
            let d = f.local(pick(k), centre)[0];
            let weight = (d.abs() / half.max(1.0)).powi(3).min(1.0);
            nudge(k, d.signum() * half * 0.12 * mouth * weight, 0.0);
        }
    }

    // Everything on the outline and the features holds still unless moved.
    let mut keys: Vec<u16> = FACE_OVAL.to_vec();
    keys.extend_from_slice(&RIGHT_EYE);
    keys.extend_from_slice(&LEFT_EYE);
    keys.extend_from_slice(&RIGHT_BROW);
    keys.extend_from_slice(&LEFT_BROW);
    keys.extend_from_slice(&NOSE_BRIDGE);
    keys.extend_from_slice(&NOSE_WINGS);
    keys.extend_from_slice(&LIPS_OUTER);
    keys.extend_from_slice(&[RIGHT_IRIS.0, LEFT_IRIS.0, 2]);
    keys.extend_from_slice(&RIGHT_IRIS.1);
    keys.extend_from_slice(&LEFT_IRIS.1);
    keys.sort_unstable();
    keys.dedup();
    let control = |k: u16| {
        let from = pick(k);
        let to = moves.get(&k).map_or(from, |m| f.shift(from, m[0], m[1]));
        Control { from, to }
    };
    let mut controls: Vec<Control> = keys.iter().map(|&k| control(k)).collect();
    // Along the outlines, points in between too: a sparse moved outline
    // would dip between its points.
    let step = (e / 150.0).max(3.0);
    let loops: [(&[u16], bool); 6] = [
        (&FACE_OVAL, true),
        (&RIGHT_EYE, true),
        (&LEFT_EYE, true),
        (&RIGHT_BROW, true),
        (&LEFT_BROW, true),
        (&LIPS_OUTER, true),
    ];
    for (outline, closed) in loops.into_iter().chain([(&NOSE_BRIDGE[..], false)]) {
        let n = outline.len();
        for i in 0..if closed { n } else { n - 1 } {
            let (a, b) = (control(outline[i]), control(outline[(i + 1) % n]));
            let length = (b.from[0] - a.from[0]).hypot(b.from[1] - a.from[1]);
            let pieces = (length / step).ceil() as usize;
            for j in 1..pieces {
                let t = j as f32 / pieces as f32;
                let mix =
                    |u: [f32; 2], v: [f32; 2]| [u[0] + (v[0] - u[0]) * t, u[1] + (v[1] - u[1]) * t];
                controls.push(Control {
                    from: mix(a.from, b.from),
                    to: mix(a.to, b.to),
                });
            }
        }
    }

    // The anchor ring: the outline pushed out by a third of the face.
    let oval = loop_points(points, &FACE_OVAL);
    let n = oval.len();
    let reach = 0.3 * e;
    let ring: Vec<[f32; 2]> = (0..n * 2)
        .map(|j| {
            let (a, b) = (oval[(j / 2) % n], oval[(j / 2 + 1) % n]);
            let t = if j % 2 == 0 { 0.0 } else { 0.5 };
            let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            let (dx, dy) = (p[0] - f.centre[0], p[1] - f.centre[1]);
            let length = dx.hypot(dy).max(1.0);
            [p[0] + dx / length * reach, p[1] + dy / length * reach]
        })
        .collect();
    controls.extend(ring.iter().map(|&p| Control::still(p)));
    let fade = 0.15 * e;
    let region = Region::around(ring.iter().copied(), [fade; 4], width, height);
    let (x0, y0) = (region.x as f32, region.y as f32);
    let (x1, y1) = (x0 + region.w as f32, y0 + region.h as f32);
    controls.extend(
        others
            .iter()
            .filter(|p| p[0] >= x0 && p[1] >= y0 && p[0] < x1 && p[1] < y1)
            .map(|&p| Control::still(p)),
    );
    Some((controls, region, fade))
}

/// `retouched` (the retouch's region and pixels over `rgba`, the analysed
/// image) with every enabled face reshaped: the changed region and its
/// pixels. Other faces hold still.
pub fn reshape_faces(
    rgba: &[u8],
    model: &PortraitModel,
    shape: &FaceShape,
    enabled: &[bool],
    retouched: Option<(Region, Vec<u8>)>,
) -> Option<(Region, Vec<u8>)> {
    if shape.is_neutral() {
        return retouched;
    }
    let (width, height) = (model.width, model.height);
    let outlines: Vec<Vec<[f32; 2]>> = model
        .faces
        .iter()
        .map(|f| loop_points(&f.mesh.points, &FACE_OVAL))
        .collect();
    let fields: Vec<Displacement> = model
        .faces
        .iter()
        .enumerate()
        .filter(|(i, _)| enabled.get(*i).copied().unwrap_or(true))
        .filter_map(|(i, face)| {
            let others: Vec<[f32; 2]> = outlines
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .flat_map(|(_, o)| o.iter().copied())
                .collect();
            face_field(&face.mesh.points, shape, &others, width, height)
        })
        .collect();
    let Some(mut region) = fields.iter().map(|f| f.region).reduce(|a, b| a.union(b)) else {
        return retouched;
    };
    if let Some((r, _)) = &retouched {
        region = region.union(*r);
    }
    // The photo over the region with the retouch laid in.
    let (rw, iw) = (region.w as usize, width as usize);
    let mut src = vec![0u8; region.len() * 4];
    src.par_chunks_mut(rw * 4)
        .enumerate()
        .for_each(|(row, line)| {
            let o = ((region.y as usize + row) * iw + region.x as usize) * 4;
            line.copy_from_slice(&rgba[o..o + rw * 4]);
        });
    if let Some((r, px)) = &retouched {
        let (dx, dy, w) = (
            (r.x - region.x) as usize,
            (r.y - region.y) as usize,
            r.w as usize,
        );
        for row in 0..r.h as usize {
            let o = ((dy + row) * rw + dx) * 4;
            src[o..o + w * 4].copy_from_slice(&px[row * w * 4..(row + 1) * w * 4]);
        }
    }
    let refs: Vec<&Displacement> = fields.iter().collect();
    let out = warp_region(&src, region, &refs, model.clip.as_ref(), region);
    Some((region, out))
}

/// The field for one face, `None` when `shape` is neutral.
pub fn face_field(
    points: &[[f32; 3]],
    shape: &FaceShape,
    others: &[[f32; 2]],
    width: u32,
    height: u32,
) -> Option<Displacement> {
    let (controls, region, fade) = face_controls(points, shape, others, width, height)?;
    let e = Frame::new(points).extent;
    Displacement::from_controls(&controls, region, (e / 100.0).max(4.0), fade)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(x: u32, y: u32, w: u32, h: u32) -> Region {
        Region { x, y, w, h }
    }

    /// One point pushed right inside a ring of still points.
    fn pushed(shift: f32) -> (Vec<Control>, Region) {
        let mut controls = vec![Control {
            from: [100.0, 100.0],
            to: [100.0 + shift, 100.0],
        }];
        for k in 0..24 {
            let a = k as f32 / 24.0 * std::f32::consts::TAU;
            controls.push(Control::still([
                100.0 + 60.0 * a.cos(),
                100.0 + 60.0 * a.sin(),
            ]));
        }
        (controls, region(20, 20, 160, 160))
    }

    #[test]
    fn rigid_mls_keeps_still_points_and_carries_moved_ones() {
        let p = [[0.0, 0.0], [10.0, 0.0], [0.0, 10.0]];
        // Identity, translation and rotation are reproduced away from the points.
        assert_eq!(mls_rigid([3.0, 4.0], &p, &p), [3.0, 4.0]);
        let t: Vec<[f32; 2]> = p.iter().map(|v| [v[0] + 5.0, v[1] - 2.0]).collect();
        let moved = mls_rigid([3.0, 4.0], &p, &t);
        assert!((moved[0] - 8.0).abs() < 1e-4 && (moved[1] - 2.0).abs() < 1e-4);
        let r: Vec<[f32; 2]> = p.iter().map(|v| [-v[1], v[0]]).collect();
        let turned = mls_rigid([3.0, 4.0], &p, &r);
        assert!((turned[0] + 4.0).abs() < 1e-3 && (turned[1] - 3.0).abs() < 1e-3);
        assert_eq!(mls_rigid([10.0, 0.0], &p, &t), t[1]);
    }

    #[test]
    fn field_takes_colour_from_where_a_point_came_from_and_nothing_past_the_ring() {
        assert!(Displacement::from_controls(&pushed(0.0).0, pushed(0.0).1, 4.0, 20.0).is_none());
        let (controls, r) = pushed(8.0);
        let field = Displacement::from_controls(&controls, r, 4.0, 20.0).unwrap();
        // The point now at x 108 shows what was at x 100.
        let o = field.offset_at(108.0, 100.0);
        assert!((o[0] + 8.0).abs() < 0.5 && o[1].abs() < 0.5, "{o:?}");
        // Still on the ring, nothing outside the region, gone at its edge.
        let ring = field.offset_at(160.0, 100.0);
        assert!(ring[0].hypot(ring[1]) < 0.3, "{ring:?}");
        assert_eq!(field.offset_at(10.0, 100.0), [0.0, 0.0]);
        let edge = field.offset_at(21.0, 100.0);
        assert!(edge[0].hypot(edge[1]) < 0.05, "{edge:?}");
        assert!(field.largest() > 7.0 && field.largest() < 9.0);
    }

    #[test]
    fn a_neutral_shape_moves_nothing() {
        let points = vec![[0.0f32; 3]; 478];
        assert!(face_controls(&points, &FaceShape::default(), &[], 100, 100).is_none());
    }

    #[test]
    fn warp_moves_the_pixels_and_a_selection_holds_them() {
        let (w, h) = (200u32, 200u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 4) as usize;
                rgba[o] = x as u8;
                rgba[o + 3] = 255;
            }
        }
        let (controls, r) = pushed(8.0);
        let field = Displacement::from_controls(&controls, r, 4.0, 20.0).unwrap();
        let whole = region(0, 0, w, h);
        let out = warp_region(&rgba, whole, &[&field], None, r);
        let at = |out: &[u8], x: u32, y: u32| out[(((y - r.y) * r.w + x - r.x) * 4) as usize];
        assert!((at(&out, 108, 100) as i32 - 100).abs() <= 1);
        assert_eq!(at(&out, 25, 25), 25);
        let none = Clip {
            region: region(0, 0, w, h),
            mask: vec![0; (w * h) as usize],
        };
        let held = warp_region(&rgba, whole, &[&field], Some(&none), r);
        assert_eq!(at(&held, 108, 100), 108);
    }
}
