//! Face-mesh outlines as ordered polygons, and feathered polygon masks.

use rayon::prelude::*;

/// Face-mesh landmark loops (MediaPipe indices). "Right" is the subject's right,
/// which sits on the image left.
pub const RIGHT_EYE: [u16; 16] = [
    33, 246, 161, 160, 159, 158, 157, 173, 133, 155, 154, 153, 145, 144, 163, 7,
];
pub const LEFT_EYE: [u16; 16] = [
    263, 466, 388, 387, 386, 385, 384, 398, 362, 382, 381, 380, 374, 373, 390, 249,
];
pub const RIGHT_EYE_LOWER: [u16; 9] = [33, 7, 163, 144, 145, 153, 154, 155, 133];
pub const LEFT_EYE_LOWER: [u16; 9] = [263, 249, 390, 373, 374, 380, 381, 382, 362];
pub const RIGHT_BROW: [u16; 10] = [46, 53, 52, 65, 55, 107, 66, 105, 63, 70];
pub const LEFT_BROW: [u16; 10] = [276, 283, 282, 295, 285, 336, 296, 334, 293, 300];
pub const MOUTH_INNER: [u16; 20] = [
    78, 95, 88, 178, 87, 14, 317, 402, 318, 324, 308, 415, 310, 311, 312, 13, 82, 81, 80, 191,
];
pub const LIPS_OUTER: [u16; 20] = [
    61, 146, 91, 181, 84, 17, 314, 405, 321, 375, 291, 409, 270, 269, 267, 0, 37, 39, 40, 185,
];
pub const FACE_OVAL: [u16; 36] = [
    10, 338, 297, 332, 284, 251, 389, 356, 454, 323, 361, 288, 397, 365, 379, 378, 400, 377, 152,
    148, 176, 149, 150, 136, 172, 58, 132, 93, 234, 127, 162, 21, 54, 103, 67, 109,
];
/// Iris centre and its four rim points.
pub const RIGHT_IRIS: (u16, [u16; 4]) = (468, [469, 470, 471, 472]);
pub const LEFT_IRIS: (u16, [u16; 4]) = (473, [474, 475, 476, 477]);
/// Nose midline from between the brows, past the eyes, down to the tip.
pub const NOSE_BRIDGE: [u16; 8] = [8, 168, 6, 197, 195, 5, 4, 1];
/// Outer edges of the nostril wings and the point below the nose tip.
pub const NOSE_WINGS: [u16; 2] = [98, 327];
pub const SUBNASALE: u16 = 2;
/// Cheek points used as the reference skin tone.
pub const CHEEKS: [u16; 6] = [50, 280, 36, 266, 205, 425];

/// A rectangle of layer pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Region {
    pub fn len(&self) -> usize {
        self.w as usize * self.h as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Index of image pixel (x, y) in this region, if it lies inside.
    pub fn index_at(&self, x: u32, y: u32) -> Option<usize> {
        (x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h)
            .then(|| ((y - self.y) * self.w + x - self.x) as usize)
    }

    /// Index in this region of pixel `i` of `inner`, a region inside it.
    pub fn index_of(&self, inner: Region, i: usize) -> usize {
        let (x, y) = (i as u32 % inner.w, i as u32 / inner.w);
        ((inner.y - self.y + y) * self.w + inner.x - self.x + x) as usize
    }

    /// The smallest region holding both.
    pub fn union(&self, other: Region) -> Region {
        if other.is_empty() {
            return *self;
        }
        if self.is_empty() {
            return other;
        }
        let (x0, y0) = (self.x.min(other.x), self.y.min(other.y));
        let x1 = (self.x + self.w).max(other.x + other.w);
        let y1 = (self.y + self.h).max(other.y + other.h);
        Region {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }

    /// The part of this region inside `other` (empty when they do not meet).
    pub fn intersect(&self, other: Region) -> Region {
        let (x0, y0) = (self.x.max(other.x), self.y.max(other.y));
        let x1 = (self.x + self.w).min(other.x + other.w);
        let y1 = (self.y + self.h).min(other.y + other.h);
        Region {
            x: x0,
            y: y0,
            w: x1.saturating_sub(x0),
            h: y1.saturating_sub(y0),
        }
    }

    /// The values of `inner`, a region inside this one, from this region's
    /// `values`.
    pub fn crop<T: Copy + Send + Sync>(&self, values: &[T], inner: Region) -> Vec<T> {
        (0..inner.len())
            .into_par_iter()
            .map(|i| values[self.index_of(inner, i)])
            .collect()
    }

    /// Bounds of `points` grown by the given margins and clipped to the image.
    pub fn around(
        points: impl Iterator<Item = [f32; 2]>,
        grow: [f32; 4],
        width: u32,
        height: u32,
    ) -> Self {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for [x, y] in points {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let [left, top, right, bottom] = grow;
        let x0 = (x0 - left).floor().clamp(0.0, width as f32) as u32;
        let y0 = (y0 - top).floor().clamp(0.0, height as f32) as u32;
        let x1 = (x1 + right).ceil().clamp(0.0, width as f32) as u32;
        let y1 = (y1 + bottom).ceil().clamp(0.0, height as f32) as u32;
        Self {
            x: x0,
            y: y0,
            w: x1.saturating_sub(x0),
            h: y1.saturating_sub(y0),
        }
    }
}

pub fn loop_points(points: &[[f32; 3]], indices: &[u16]) -> Vec<[f32; 2]> {
    indices
        .iter()
        .map(|&i| {
            let p = points[i as usize];
            [p[0], p[1]]
        })
        .collect()
}

/// Positive inside `poly`, negative outside: distance to the nearest edge.
pub(super) fn signed_distance(poly: &[[f32; 2]], x: f32, y: f32) -> f32 {
    let mut inside = false;
    let mut nearest = f32::MAX;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let [ax, ay] = poly[j];
        let [bx, by] = poly[i];
        if (by > y) != (ay > y) && x < (ax - bx) * (y - by) / (ay - by) + bx {
            inside = !inside;
        }
        let (dx, dy) = (ax - bx, ay - by);
        let len_sq = dx * dx + dy * dy;
        let t = if len_sq > 0.0 {
            (((x - bx) * dx + (y - by) * dy) / len_sq).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (ex, ey) = (x - (bx + t * dx), y - (by + t * dy));
        nearest = nearest.min(ex * ex + ey * ey);
        j = i;
    }
    let distance = nearest.sqrt();
    if inside {
        distance
    } else {
        -distance
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Coverage of `poly` grown by `grow` px with a soft edge `feather` px wide,
/// max-combined into a region-sized mask.
pub fn stamp_polygon(mask: &mut [f32], region: Region, poly: &[[f32; 2]], grow: f32, feather: f32) {
    if poly.len() < 3 || region.is_empty() {
        return;
    }
    let half = feather.max(0.5) * 0.5;
    let pad = grow.max(0.0) + half + 1.0;
    let bounds = Region::around(
        poly.iter().copied(),
        [pad; 4],
        region.x + region.w,
        region.y + region.h,
    );
    let x0 = bounds.x.max(region.x);
    let y0 = bounds.y.max(region.y);
    let x1 = (bounds.x + bounds.w).min(region.x + region.w);
    let y1 = (bounds.y + bounds.h).min(region.y + region.h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    mask.par_chunks_mut(region.w as usize)
        .enumerate()
        .skip((y0 - region.y) as usize)
        .take((y1 - y0) as usize)
        .for_each(|(row, line)| {
            let y = (region.y as usize + row) as f32 + 0.5;
            for x in x0..x1 {
                let d = signed_distance(poly, x as f32 + 0.5, y) + grow;
                let coverage = smoothstep(-half, half, d);
                let cell = &mut line[(x - region.x) as usize];
                *cell = cell.max(coverage);
            }
        });
}

/// Soft disc, max-combined into a region-sized mask.
pub fn stamp_disc(mask: &mut [f32], region: Region, centre: [f32; 2], radius: f32, feather: f32) {
    let ring: Vec<[f32; 2]> = (0..32)
        .map(|i| {
            let a = i as f32 / 32.0 * std::f32::consts::TAU;
            [centre[0] + radius * a.cos(), centre[1] + radius * a.sin()]
        })
        .collect();
    stamp_polygon(mask, region, &ring, 0.0, feather);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamped_square_is_solid_inside_and_soft_at_the_edge() {
        let region = Region {
            x: 0,
            y: 0,
            w: 40,
            h: 40,
        };
        let mut mask = vec![0.0; region.len()];
        let square = [[10.0, 10.0], [30.0, 10.0], [30.0, 30.0], [10.0, 30.0]];
        stamp_polygon(&mut mask, region, &square, 0.0, 4.0);
        assert!(mask[20 * 40 + 20] > 0.99);
        assert!(mask[2 * 40 + 2] < 0.01);
        let edge = mask[20 * 40 + 10];
        assert!(edge > 0.2 && edge < 0.8, "edge coverage {edge}");
    }

    #[test]
    fn inner_regions_map_into_outer_ones() {
        let outer = Region {
            x: 10,
            y: 20,
            w: 30,
            h: 25,
        };
        let inner = Region {
            x: 15,
            y: 22,
            w: 8,
            h: 6,
        };
        let values: Vec<usize> = (0..outer.len()).collect();
        let cropped = outer.crop(&values, inner);
        assert_eq!(cropped[0], outer.index_at(15, 22).unwrap());
        assert_eq!(cropped[9], outer.index_at(16, 23).unwrap());
        assert_eq!(outer.index_at(40, 22), None);
        assert_eq!(outer.union(inner), outer);
        let wide = inner.union(Region {
            x: 30,
            y: 40,
            w: 5,
            h: 5,
        });
        assert_eq!((wide.x, wide.y, wide.w, wide.h), (15, 22, 20, 23));
    }
}
