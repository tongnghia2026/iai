// Selection mask with a cached bounding box.
//
// Important (bug fix B5 + C1):
// - bbox is cached and only recomputed when the mask changes
// - previously recomputed O(W*H) every frame in push_selection_uniforms()
// - now: dirty flag -> recompute once when needed, cached for later frames

use rayon::prelude::*;

/// Check in parallel whether the mask has any selected pixel. Replaces the
/// single-threaded `mask.iter().any(|&v| v > 0)` run after EVERY quick-select /
/// refine-brush stamp, which scanned the whole canvas (O(W*H)) and stuttered on large images.
#[inline]
pub fn mask_has_any(mask: &[u8]) -> bool {
    mask.par_iter().any(|&v| v > 0)
}

#[inline]
fn mask_len(width: u32, height: u32) -> Option<usize> {
    (width as usize).checked_mul(height as usize)
}

/// How an external mask combines into the current selection
/// (Channels panel "Load Channel as Selection").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskCombine {
    Replace,
    Add,
    Subtract,
    Intersect,
}

#[derive(Clone)]
pub struct Selection {
    pub mask: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub active: bool,
    pub offset: (i32, i32),

    bbox_dirty: bool,
    cached_x0: u32,
    cached_y0: u32,
    cached_x1: u32,
    cached_y1: u32,

    /// Incremented every time mask pixel data changes.
    /// Allows callers to skip GPU texture upload when mask hasn't changed.
    pub mask_revision: u64,
}

impl Selection {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            mask: vec![0u8; mask_len(width, height).unwrap_or(0)],
            width,
            height,
            active: false,
            offset: (0, 0),
            bbox_dirty: true,
            cached_x0: 0,
            cached_y0: 0,
            cached_x1: 0,
            cached_y1: 0,
            mask_revision: 0,
        }
    }

    /// Resident heap bytes of the selection mask (one byte per canvas pixel).
    /// Memory Milestone M0 — allocated at full canvas size even when inactive,
    /// which M2 later makes lazy.
    pub fn resident_bytes(&self) -> u64 {
        self.mask.len() as u64
    }

    pub fn select_all(&mut self) {
        self.mask.fill(255);
        self.active = true;
        self.offset = (0, 0);
        self.cached_x0 = 0;
        self.cached_y0 = 0;
        self.cached_x1 = self.width;
        self.cached_y1 = self.height;
        self.bbox_dirty = false;
        self.mask_revision += 1;
    }

    pub fn deselect(&mut self) {
        self.mask.fill(0);
        self.active = false;
        self.offset = (0, 0);
        self.bbox_dirty = false;
        self.cached_x0 = 0;
        self.cached_y0 = 0;
        self.cached_x1 = 0;
        self.cached_y1 = 0;
        self.mask_revision += 1;
    }

    pub fn invert(&mut self) {
        self.mask.par_iter_mut().for_each(|p| *p = 255 - *p);
        self.active = self.mask.par_iter().any(|&p| p > 0);
        self.bbox_dirty = true;
        self.mask_revision += 1;
    }

    pub fn select_rect(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        self.mask.fill(0);
        self.offset = (0, 0);
        let x1c = x1.min(self.width);
        let y1c = y1.min(self.height);
        for y in y0..y1c {
            for x in x0..x1c {
                let i = (y * self.width + x) as usize;
                if i < self.mask.len() {
                    self.mask[i] = 255;
                }
            }
        }
        self.active = x1c > x0 && y1c > y0;
        if self.active {
            self.cached_x0 = x0;
            self.cached_y0 = y0;
            self.cached_x1 = x1c;
            self.cached_y1 = y1c;
            self.bbox_dirty = false;
        } else {
            self.bbox_dirty = false;
        }
        self.mask_revision += 1;
    }

    /// Combine an external canvas-space mask into the selection (Channels
    /// panel "Load Channel as Selection"). The result is rebuilt in canvas
    /// space (offset reset); soft edges combine with max / product / min, the
    /// common raster-editor forms. With no active selection Add behaves like
    /// Replace, Subtract/Intersect leave nothing (the UI offers them only
    /// with a live selection).
    pub fn combine_with_mask(&mut self, mask: &[u8], w: u32, h: u32, mode: MaskCombine) {
        if w == 0 || h == 0 || mask.len() < (w as usize) * (h as usize) {
            return;
        }
        let was_active = self.active;
        let mut next = vec![0u8; (w as usize) * (h as usize)];
        for y in 0..h {
            for x in 0..w {
                let i = (y as usize) * (w as usize) + (x as usize);
                let ch = mask[i] as f32 / 255.0;
                let cur = if was_active { self.sample(x, y) } else { 0.0 };
                let out = match mode {
                    MaskCombine::Replace => ch,
                    MaskCombine::Add => cur.max(ch),
                    MaskCombine::Subtract => cur * (1.0 - ch),
                    MaskCombine::Intersect => cur.min(ch),
                };
                next[i] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        self.mask = next;
        self.width = w;
        self.height = h;
        self.offset = (0, 0);
        self.active = self.mask.iter().any(|&p| p > 0);
        self.bbox_dirty = true;
        self.mask_revision += 1;
    }

    /// Paint an extra region into the selection mask (used by the lasso tool).
    #[allow(dead_code)]
    pub fn paint_mask_region(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, value: u8) {
        let x1c = x1.min(self.width);
        let y1c = y1.min(self.height);
        for y in y0..y1c {
            for x in x0..x1c {
                let i = (y * self.width + x) as usize;
                if i < self.mask.len() {
                    self.mask[i] = value;
                }
            }
        }
        self.active = value > 0 || self.mask.iter().any(|&p| p > 0);
        self.bbox_dirty = true;
        self.mask_revision += 1;
    }

    /// Is the pixel selected? With no selection, every pixel counts as selected.
    #[inline]
    pub fn is_selected(&self, x: u32, y: u32) -> bool {
        if !self.active {
            return true;
        }
        let sx = x as i32 - self.offset.0;
        let sy = y as i32 - self.offset.1;
        if sx >= 0 && sy >= 0 && sx < self.width as i32 && sy < self.height as i32 {
            let i = (sy * self.width as i32 + sx) as usize;
            i < self.mask.len() && self.mask[i] > 127
        } else {
            false
        }
    }

    /// Sample the selection alpha at a pixel (for soft selection).
    #[inline]
    pub fn sample(&self, x: u32, y: u32) -> f32 {
        if !self.active {
            return 1.0;
        }
        let sx = x as i32 - self.offset.0;
        let sy = y as i32 - self.offset.1;
        if sx >= 0 && sy >= 0 && sx < self.width as i32 && sy < self.height as i32 {
            let i = (sy * self.width as i32 + sx) as usize;
            if i < self.mask.len() {
                self.mask[i] as f32 / 255.0
            } else {
                0.0
            }
        } else {
            0.0
        }
    }

    /// Selection bounding box (x0, y0, x1, y1) as f32.
    /// CACHED — O(1) after the first call.
    pub fn bounding_box(&mut self) -> (f32, f32, f32, f32) {
        if !self.active {
            return (0.0, 0.0, 0.0, 0.0);
        }
        if self.bbox_dirty {
            self.recompute_bbox();
        }
        (
            (self.cached_x0 as i32 + self.offset.0) as f32,
            (self.cached_y0 as i32 + self.offset.1) as f32,
            (self.cached_x1 as i32 + self.offset.0) as f32,
            (self.cached_y1 as i32 + self.offset.1) as f32,
        )
    }

    /// Bounding box without &mut (read-only use; cache may be stale).
    ///
    /// No longer returns (0,0,0,0) when bbox_dirty.
    /// A stale cached value beats zero: zero makes the marching ants
    /// jump to the canvas top-left, while a stale value is only slightly off
    /// (and only until the next frame, when refresh_bbox() runs).
    ///
    /// Callers needing an exact value must call refresh_bbox() first
    /// or use bounding_box() (auto-refresh, takes &mut self).
    pub fn bounding_box_cached(&self) -> (f32, f32, f32, f32) {
        if !self.active {
            return (0.0, 0.0, 0.0, 0.0);
        }
        (
            (self.cached_x0 as i32 + self.offset.0) as f32,
            (self.cached_y0 as i32 + self.offset.1) as f32,
            (self.cached_x1 as i32 + self.offset.0) as f32,
            (self.cached_y1 as i32 + self.offset.1) as f32,
        )
    }

    /// Refresh the bbox cache now (call after modifying the mask if the bbox is needed now).
    pub fn refresh_bbox(&mut self) {
        if self.bbox_dirty {
            self.recompute_bbox();
        }
    }

    pub fn mark_bbox_dirty(&mut self) {
        self.bbox_dirty = true;
    }

    /// True when the whole bounding box is solidly selected — i.e. the selection
    /// is an axis-aligned rectangle (a marquee), not a lasso/ellipse/feathered
    /// mask. Gates vector Trim, which in v1 only cuts a rectangular region (any
    /// other shape would over-cut to its bounding box). Rectangularity is a
    /// property of the mask itself, so `offset` is irrelevant and the cached
    /// mask-space bbox is scanned directly. O(bbox area); runs on a Delete
    /// keypress, not per frame.
    pub fn is_solid_rect(&mut self) -> bool {
        if !self.active {
            return false;
        }
        if self.bbox_dirty {
            self.recompute_bbox();
        }
        let (x0, y0, x1, y1) = (
            self.cached_x0,
            self.cached_y0,
            self.cached_x1,
            self.cached_y1,
        );
        if x1 <= x0 || y1 <= y0 {
            return false;
        }
        let w = self.width;
        for y in y0..y1 {
            let row = (y * w) as usize;
            for x in x0..x1 {
                let i = row + x as usize;
                if i >= self.mask.len() || self.mask[i] <= 127 {
                    return false;
                }
            }
        }
        true
    }

    fn recompute_bbox(&mut self) {
        use rayon::prelude::*;
        let w = self.width as usize;
        let h = self.height as usize;
        let mask = &self.mask;

        let min_y = (0..h)
            .into_par_iter()
            .find_first(|&y| mask[y * w..(y + 1) * w].iter().any(|&v| v > 0))
            .unwrap_or(h);
        let max_y = (0..h)
            .into_par_iter()
            .find_last(|&y| mask[y * w..(y + 1) * w].iter().any(|&v| v > 0))
            .unwrap_or(0);

        let (min_x, max_x) = mask
            .par_chunks(w)
            .filter(|row| row.iter().any(|&v| v > 0))
            .map(|row| {
                let x0 = row.iter().position(|&v| v > 0).unwrap_or(w);
                let x1 = row.iter().rposition(|&v| v > 0).unwrap_or(0);
                (x0, x1)
            })
            .reduce(|| (w, 0), |(a0, a1), (b0, b1)| (a0.min(b0), a1.max(b1)));

        if min_y < h {
            self.cached_x0 = min_x as u32;
            self.cached_y0 = min_y as u32;
            self.cached_x1 = (max_x + 1) as u32;
            self.cached_y1 = (max_y + 1) as u32;
        } else {
            self.cached_x0 = 0;
            self.cached_y0 = 0;
            self.cached_x1 = 0;
            self.cached_y1 = 0;
            self.active = false;
        }
        self.bbox_dirty = false;
    }

    /// Resize selection khi canvas resize
    pub fn resize(&mut self, new_w: u32, new_h: u32) {
        self.mask = vec![0u8; mask_len(new_w, new_h).unwrap_or(0)];
        self.width = new_w;
        self.height = new_h;
        self.active = false;
        self.bbox_dirty = false;
        self.cached_x0 = 0;
        self.cached_y0 = 0;
        self.cached_x1 = 0;
        self.cached_y1 = 0;
        self.mask_revision += 1;
    }
}

/// Flood-fill the selection mask from point (sx, sy).
///
/// Edge-aware algorithm — combines two conditions to expand the BFS:
///   1. **Perceptual color distance** from the seed (luminance-weighted Euclidean, not avg RGBA)
///   2. **Local gradient stop**: stop at edges with high brightness contrast
///      (edge_sensitivity 0 = off, 100 = stop at every edge)
///
/// Good for portraits / full body: the BFS crosses internal colors (clothes, skin, hair)
/// but stops at the person-background border when the brightness gradient is strong enough.
#[allow(dead_code)]
pub fn flood_fill_mask(
    pixels: &[u8],
    width: u32,
    height: u32,
    sx: u32,
    sy: u32,
    tolerance: u8,
    edge_sensitivity: u8,
    contiguous: bool,
    anti_alias: bool,
) -> Vec<u8> {
    let Some(n) = mask_len(width, height) else {
        return Vec::new();
    };
    let mut mask = vec![0u8; n];
    if width == 0 || height == 0 || sx >= width || sy >= height {
        return mask;
    }

    let get_px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        if i + 3 < pixels.len() {
            [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
        } else {
            [0, 0, 0, 0]
        }
    };

    let color_dist = |a: [u8; 4], b: [u8; 4]| -> f32 {
        let r = a[0] as f32 - b[0] as f32;
        let g = a[1] as f32 - b[1] as f32;
        let bl = a[2] as f32 - b[2] as f32;
        let al = a[3] as f32 - b[3] as f32;
        (0.30 * r * r + 0.59 * g * g + 0.11 * bl * bl + 0.10 * al * al).sqrt()
    };

    let lum =
        |c: [u8; 4]| -> f32 { 0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32 };

    let seed = get_px(sx, sy);
    let tol = tolerance as f32;
    let edge_thr = if edge_sensitivity == 0 {
        f32::MAX
    } else {
        255.0 * (1.0 - edge_sensitivity as f32 / 100.0)
    };

    if contiguous {
        let mut visited = vec![false; n];
        let mut queue = std::collections::VecDeque::new();
        visited[(sy * width + sx) as usize] = true;
        queue.push_back((sx, sy));

        while let Some((x, y)) = queue.pop_front() {
            let curr = get_px(x, y);
            if color_dist(curr, seed) > tol {
                continue;
            }
            mask[(y * width + x) as usize] = 255;

            let neighbors = [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ];
            for (nx, ny) in neighbors {
                if nx >= width || ny >= height {
                    continue;
                }
                let ni = (ny * width + nx) as usize;
                if visited[ni] {
                    continue;
                }
                visited[ni] = true;

                let next = get_px(nx, ny);
                let color_ok = color_dist(next, seed) <= tol;
                let edge_ok = (lum(curr) - lum(next)).abs() < edge_thr;

                if color_ok && edge_ok {
                    queue.push_back((nx, ny));
                }
            }
        }
    } else {
        for y in 0..height {
            for x in 0..width {
                if color_dist(get_px(x, y), seed) <= tol {
                    mask[(y * width + x) as usize] = 255;
                }
            }
        }
    }

    if anti_alias {
        let aa_dist = tol * 0.15 + 15.0;
        let snap = mask.clone();
        for y in 0..height {
            for x in 0..width {
                let i = (y * width + x) as usize;
                if snap[i] != 0 {
                    continue;
                }

                let near_sel = [
                    (x.wrapping_sub(1), y),
                    (x + 1, y),
                    (x, y.wrapping_sub(1)),
                    (x, y + 1),
                ]
                .iter()
                .any(|&(nx, ny)| {
                    nx < width && ny < height && snap[(ny * width + nx) as usize] == 255
                });
                if !near_sel {
                    continue;
                }

                let px = get_px(x, y);
                let cd = color_dist(px, seed);
                if cd <= tol + aa_dist {
                    let t = 1.0 - (cd - tol).max(0.0) / aa_dist;
                    mask[i] = (t * 255.0).clamp(0.0, 255.0) as u8;
                }
            }
        }
    }

    mask
}

/// Luminance-weighted RGB distance between two colours (0 = identical,
/// ~255 = opposite). Shared by [`color_range_mask`] and the dialog preview so
/// both grade a pixel identically.
#[inline]
pub fn color_range_distance(a: [u8; 3], b: [u8; 3]) -> f32 {
    let r = a[0] as f32 - b[0] as f32;
    let g = a[1] as f32 - b[1] as f32;
    let bl = a[2] as f32 - b[2] as f32;
    (0.30 * r * r + 0.59 * g * g + 0.11 * bl * bl).sqrt()
}

/// Soft selection alpha (0..=255) for a pixel, given the sampled `target` colour
/// and a `tolerance` in distance units. Fully selected at distance 0, fading
/// linearly to 0 at `tolerance` — the feathered edge that makes Color Range read
/// like Photoshop's Fuzziness.
#[inline]
pub fn color_range_alpha(px: [u8; 3], target: [u8; 3], tolerance: f32) -> u8 {
    let tol = tolerance.max(1.0);
    let d = color_range_distance(px, target);
    let a = (1.0 - d / tol).clamp(0.0, 1.0);
    (a * 255.0).round() as u8
}

/// Build a soft selection mask from colour similarity to `target`
/// (Select ▸ Color Range). `fuzziness` (0..=200) is the luminance-weighted RGB
/// distance at which a pixel fades from fully selected to unselected. Alpha is
/// `1 - d / fuzziness`, so a small fuzziness picks only near-identical colours
/// and a large one grabs a broad family with a feathered edge. `pixels` is the
/// canvas-space RGBA buffer (the composited image when sampling merged).
pub fn color_range_mask(
    pixels: &[u8],
    width: u32,
    height: u32,
    target: [u8; 3],
    fuzziness: u8,
) -> Vec<u8> {
    let Some(n) = mask_len(width, height) else {
        return Vec::new();
    };
    let mut mask = vec![0u8; n];
    if pixels.len() < n * 4 {
        return mask;
    }
    let tol = fuzziness as f32;
    mask.par_iter_mut().enumerate().for_each(|(i, out)| {
        let p = i * 4;
        let px = [pixels[p], pixels[p + 1], pixels[p + 2]];
        *out = color_range_alpha(px, target, tol);
    });
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_range_selects_matching_colours_and_fades_with_distance() {
        // 3 pixels: exact match, a near colour, and a far colour.
        let target = [200u8, 50, 50];
        let pixels: Vec<u8> = vec![
            200, 50, 50, 255, // exact
            190, 60, 55, 255, // close
            10, 220, 30, 255, // far (green)
        ];
        let mask = color_range_mask(&pixels, 3, 1, target, 40);
        assert_eq!(mask[0], 255, "exact match fully selected");
        assert!(
            mask[1] > 0 && mask[1] < 255,
            "near colour partially selected"
        );
        assert_eq!(mask[2], 0, "far colour not selected");
    }

    #[test]
    fn color_range_zero_fuzziness_keeps_only_exact() {
        let target = [120u8, 120, 120];
        let pixels: Vec<u8> = vec![120, 120, 120, 255, 122, 120, 120, 255];
        let mask = color_range_mask(&pixels, 2, 1, target, 0);
        assert_eq!(mask[0], 255);
        assert_eq!(mask[1], 0);
    }

    #[test]
    fn bbox_is_cached() {
        let mut sel = Selection::new(100, 100);
        sel.select_rect(10, 20, 50, 60);
        let (x0, y0, x1, y1) = sel.bounding_box();
        assert_eq!((x0, y0, x1, y1), (10.0, 20.0, 50.0, 60.0));
        let (x0b, y0b, x1b, y1b) = sel.bounding_box();
        assert_eq!((x0b, y0b, x1b, y1b), (10.0, 20.0, 50.0, 60.0));
    }

    #[test]
    fn combine_with_mask_modes() {
        // Channel mask: left half selected (255), right half empty.
        let w = 10u32;
        let h = 4u32;
        let mut channel = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..5 {
                channel[(y * w + x) as usize] = 255;
            }
        }

        // Replace with no prior selection.
        let mut sel = Selection::new(w, h);
        sel.combine_with_mask(&channel, w, h, MaskCombine::Replace);
        assert!(sel.active);
        assert_eq!(sel.sample(0, 0), 1.0);
        assert_eq!(sel.sample(9, 0), 0.0);

        // Add onto a right-side rect: both halves selected.
        let mut sel = Selection::new(w, h);
        sel.select_rect(5, 0, 10, 4);
        sel.combine_with_mask(&channel, w, h, MaskCombine::Add);
        assert_eq!(sel.sample(0, 0), 1.0);
        assert_eq!(sel.sample(9, 0), 1.0);

        // Subtract the channel from a full selection: only the right stays.
        let mut sel = Selection::new(w, h);
        sel.select_all();
        sel.combine_with_mask(&channel, w, h, MaskCombine::Subtract);
        assert_eq!(sel.sample(0, 0), 0.0);
        assert_eq!(sel.sample(9, 0), 1.0);

        // Intersect a right-side rect with the left-side channel: nothing.
        let mut sel = Selection::new(w, h);
        sel.select_rect(5, 0, 10, 4);
        sel.combine_with_mask(&channel, w, h, MaskCombine::Intersect);
        assert!(!sel.active);

        // Add with no prior selection behaves like replace; the selection's
        // offset is resolved into canvas space first.
        let mut sel = Selection::new(w, h);
        sel.select_rect(5, 0, 10, 4);
        sel.offset = (2, 0); // shifted right by 2 (7..10 +wrap-less)
        sel.combine_with_mask(&channel, w, h, MaskCombine::Add);
        assert_eq!(sel.offset, (0, 0));
        assert_eq!(
            sel.sample(6, 0),
            0.0,
            "outside both shifted rect and channel"
        );
        assert_eq!(sel.sample(7, 0), 1.0, "inside the shifted rect");
    }

    #[test]
    fn invert_marks_dirty() {
        let mut sel = Selection::new(10, 10);
        sel.select_rect(2, 2, 8, 8);
        sel.invert();
        assert!(sel.bbox_dirty);
        let (x0, _, x1, _) = sel.bounding_box();
        assert_eq!(x0, 0.0);
        assert_eq!(x1, 10.0);
    }

    #[test]
    fn deselect_clears_bbox() {
        let mut sel = Selection::new(100, 100);
        sel.select_all();
        sel.deselect();
        let (x0, y0, x1, y1) = sel.bounding_box();
        assert_eq!((x0, y0, x1, y1), (0.0, 0.0, 0.0, 0.0));
    }
}

#[derive(Clone)]
pub struct SelectionSnapshot {
    pub(crate) mask: Vec<u8>,
    pub(crate) active: bool,
    pub(crate) offset: (i32, i32),
}

impl Selection {
    pub fn snapshot(&self) -> SelectionSnapshot {
        SelectionSnapshot {
            mask: self.mask.clone(),
            active: self.active,
            offset: self.offset,
        }
    }

    pub fn restore_snapshot(&mut self, snap: &SelectionSnapshot) {
        self.mask.clone_from(&snap.mask);
        self.active = snap.active;
        self.offset = snap.offset;
        self.bbox_dirty = true;
        self.mask_revision += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SelectionMode {
    #[default]
    New,
    Add,
    Subtract,
    Intersect,
}

impl Selection {
    /// Apply a new mask according to the selection mode (New/Add/Subtract/Intersect).
    ///
    /// Add/Subtract/Intersect call commit_offset() before the boolean op, ensuring
    /// both masks share the same coordinate space.
    /// Previously, if offset != (0,0) (from arrow keys or the Move tool), the current mask
    /// was displayed at the offset position but Add/Subtract operated on
    /// data at offset=0 → a misplaced result.
    pub fn apply_with_mode(&mut self, new_mask: Vec<u8>, mode: SelectionMode) {
        assert_eq!(new_mask.len(), self.mask.len(), "mask size mismatch");
        match mode {
            SelectionMode::New => {
                self.mask = new_mask;
                self.offset = (0, 0);
            }
            SelectionMode::Add => {
                self.commit_offset();
                self.mask
                    .par_iter_mut()
                    .zip(new_mask.par_iter())
                    .for_each(|(a, b)| {
                        if *b > 0 {
                            *a = 255;
                        }
                    });
            }
            SelectionMode::Subtract => {
                self.commit_offset();
                self.mask
                    .par_iter_mut()
                    .zip(new_mask.par_iter())
                    .for_each(|(a, b)| {
                        if *b > 0 {
                            *a = 0;
                        }
                    });
            }
            SelectionMode::Intersect => {
                self.commit_offset();
                self.mask
                    .par_iter_mut()
                    .zip(new_mask.par_iter())
                    .for_each(|(a, b)| {
                        *a = ((*a as u16 * *b as u16) / 255) as u8;
                    });
            }
        }
        self.active = self.mask.par_iter().any(|&v| v > 0);
        self.bbox_dirty = true;
        self.mask_revision += 1;
    }

    /// Fill an axis-aligned ellipse inscribed in the bounding rect (x0,y0)-(x1,y1).
    pub fn build_ellipse_mask(
        &self,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
        feather: f32,
        anti_alias: bool,
    ) -> Vec<u8> {
        let mut mask = vec![0u8; self.mask.len()];
        if x1 <= x0 || y1 <= y0 {
            return mask;
        }

        let cx = (x0 + x1) as f32 * 0.5;
        let cy = (y0 + y1) as f32 * 0.5;
        let a = (x1 - x0) as f32 * 0.5;
        let b = (y1 - y0) as f32 * 0.5;
        if a < 0.5 || b < 0.5 {
            return mask;
        }

        let x0c = x0.min(self.width);
        let y0c = y0.min(self.height);
        let x1c = x1.min(self.width);
        let y1c = y1.min(self.height);

        for py in y0c..y1c {
            for px in x0c..x1c {
                let dx = px as f32 + 0.5 - cx;
                let dy = py as f32 + 0.5 - cy;
                let val = (dx * dx) / (a * a) + (dy * dy) / (b * b);

                if feather == 0.0 {
                    if val <= 1.0 {
                        if anti_alias {
                            let sub_samples =
                                [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];
                            let mut count = 0.0;
                            for (sx, sy) in sub_samples {
                                let spx = px as f32 + sx;
                                let spy = py as f32 + sy;
                                let sdx = spx - cx;
                                let sdy = spy - cy;
                                if (sdx * sdx) / (a * a) + (sdy * sdy) / (b * b) <= 1.0 {
                                    count += 1.0;
                                }
                            }
                            mask[(py * self.width + px) as usize] = (count / 4.0 * 255.0) as u8;
                        } else {
                            mask[(py * self.width + px) as usize] = 255;
                        }
                    }
                } else {
                    if val <= 1.0 {
                        let dist_inside = (1.0 - val.sqrt()) * a.min(b);
                        let alpha = (dist_inside / feather).clamp(0.0, 1.0);
                        mask[(py * self.width + px) as usize] = (alpha * 255.0) as u8;
                    }
                }
            }
        }
        mask
    }

    pub fn select_ellipse_mode(
        &mut self,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
        mode: SelectionMode,
        feather: f32,
        anti_alias: bool,
    ) {
        let mask = self.build_ellipse_mask(x0, y0, x1, y1, feather, anti_alias);
        self.apply_with_mode(mask, mode);
    }

    /// Scanline fill of an arbitrary polygon (canvas-space float coords).
    pub fn build_polygon_mask(&self, points: &[(f32, f32)]) -> Vec<u8> {
        let mut mask = vec![0u8; self.mask.len()];
        if points.len() < 3 {
            return mask;
        }

        let w = self.width as i32;
        let h = self.height as i32;

        for y in 0..h {
            let mut xs: Vec<i32> = Vec::new();
            let n = points.len();
            for i in 0..n {
                let (x1, y1) = points[i];
                let (x2, y2) = points[(i + 1) % n];
                let yf = y as f32 + 0.5;
                if (y1 < yf && y2 >= yf) || (y2 < yf && y1 >= yf) {
                    let t = (yf - y1) / (y2 - y1);
                    let xi = (x1 + t * (x2 - x1)) as i32;
                    xs.push(xi);
                }
            }
            xs.sort_unstable();

            let mut i = 0;
            while i + 1 < xs.len() {
                let xa = xs[i].max(0);
                let xb = xs[i + 1].min(w - 1);
                for x in xa..=xb {
                    mask[(y * w + x) as usize] = 255;
                }
                i += 2;
            }
        }
        mask
    }

    /// Draws a 1-pixel thick path for live preview of tools like Lasso.
    #[allow(dead_code)]
    pub fn build_path_mask(&self, points: &[(f32, f32)]) -> Vec<u8> {
        let mut mask = vec![0u8; self.mask.len()];
        if points.is_empty() {
            return mask;
        }
        let w = self.width as i32;
        let h = self.height as i32;

        for i in 0..points.len() - 1 {
            let mut x = points[i].0 as i32;
            let mut y = points[i].1 as i32;
            let x1 = points[i + 1].0 as i32;
            let y1 = points[i + 1].1 as i32;

            let dx = (x1 - x).abs();
            let sx = if x < x1 { 1 } else { -1 };
            let dy = -(y1 - y).abs();
            let sy = if y < y1 { 1 } else { -1 };
            let mut err = dx + dy;

            loop {
                if x >= 0 && x < w && y >= 0 && y < h {
                    mask[(y * w + x) as usize] = 255;
                }
                if x == x1 && y == y1 {
                    break;
                }
                let e2 = 2 * err;
                if e2 >= dy {
                    err += dy;
                    x += sx;
                }
                if e2 <= dx {
                    err += dx;
                    y += sy;
                }
            }
        }
        mask
    }

    #[allow(dead_code)]
    pub fn select_polygon_mode(&mut self, points: &[(f32, f32)], mode: SelectionMode) {
        let mask = self.build_polygon_mask(points);
        self.apply_with_mode(mask, mode);
    }

    /// Expand selection by `pixels` using BFS — O(N) regardless of radius.
    /// Replaces N-pass dilation (was O(N × radius), freezes for large radius).
    pub fn grow(&mut self, pixels: u32) {
        if pixels == 0 || !self.active {
            return;
        }
        let w = self.width as usize;
        let h = self.height as usize;
        let max_dist = pixels as i32;
        let max_dist_sq = max_dist * max_dist;

        let original = self.mask.clone();
        let mut min_dist_sq = vec![i32::MAX; w * h];
        let mut queue = std::collections::VecDeque::with_capacity(w.max(h) * 4);

        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if original[i] == 0 {
                    continue;
                }
                let on_boundary = (x == 0 || original[i - 1] == 0)
                    || (x + 1 == w || original[i + 1] == 0)
                    || (y == 0 || original[i - w] == 0)
                    || (y + 1 == h || original[i + w] == 0);
                if on_boundary {
                    queue.push_back((x as i32, y as i32, x as i32, y as i32));
                    min_dist_sq[i] = 0;
                }
            }
        }

        while let Some((cx, cy, sx, sy)) = queue.pop_front() {
            for (dx, dy) in [(-1i32, 0), (1, 0), (0, -1i32), (0, 1)] {
                let nx = cx + dx;
                let ny = cy + dy;
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }

                let dist_sq = (nx - sx) * (nx - sx) + (ny - sy) * (ny - sy);
                if dist_sq > max_dist_sq {
                    continue;
                }

                let ni = ny as usize * w + nx as usize;
                if dist_sq < min_dist_sq[ni] {
                    min_dist_sq[ni] = dist_sq;
                    self.mask[ni] = 255;
                    queue.push_back((nx, ny, sx, sy));
                }
            }
        }

        self.bbox_dirty = true;
        self.mask_revision += 1;
    }

    /// Contract selection by `pixels` using BFS — O(N) regardless of radius.
    pub fn shrink(&mut self, pixels: u32) {
        if pixels == 0 || !self.active {
            return;
        }
        let w = self.width as usize;
        let h = self.height as usize;
        let max_dist = pixels as i32;
        let max_dist_sq = max_dist * max_dist;

        let original = self.mask.clone();
        let mut min_dist_sq = vec![i32::MAX; w * h];
        let mut queue = std::collections::VecDeque::with_capacity(w.max(h) * 4);

        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if original[i] == 0 {
                    continue;
                }

                for (dx, dy) in [(-1i32, 0), (1, 0), (0, -1i32), (0, 1)] {
                    let sx = x as i32 + dx;
                    let sy = y as i32 + dy;
                    let outside = sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32;
                    let unselected = !outside && original[sy as usize * w + sx as usize] == 0;
                    if !outside && !unselected {
                        continue;
                    }

                    let dist_sq =
                        (x as i32 - sx) * (x as i32 - sx) + (y as i32 - sy) * (y as i32 - sy);
                    if dist_sq <= max_dist_sq && dist_sq < min_dist_sq[i] {
                        min_dist_sq[i] = dist_sq;
                        self.mask[i] = 0;
                        queue.push_back((x as i32, y as i32, sx, sy));
                    }
                }
            }
        }

        while let Some((cx, cy, sx, sy)) = queue.pop_front() {
            for (dx, dy) in [(-1i32, 0), (1, 0), (0, -1i32), (0, 1)] {
                let nx = cx + dx;
                let ny = cy + dy;
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }

                let ni = ny as usize * w + nx as usize;
                if original[ni] == 0 {
                    continue;
                }

                let dist_sq = (nx - sx) * (nx - sx) + (ny - sy) * (ny - sy);
                if dist_sq > max_dist_sq {
                    continue;
                }

                if dist_sq < min_dist_sq[ni] {
                    min_dist_sq[ni] = dist_sq;
                    self.mask[ni] = 0;
                    queue.push_back((nx, ny, sx, sy));
                }
            }
        }

        self.active = self.mask.iter().any(|&p| p > 0);
        self.bbox_dirty = true;
        self.mask_revision += 1;
    }

    /// Soften selection edges with 3 passes of box blur (approximates gaussian).
    /// `radius` is the blur kernel half-size in pixels.
    pub fn feather(&mut self, radius: f32) {
        if radius < 0.5 || !self.active {
            return;
        }
        blur_mask(
            &mut self.mask,
            self.width as usize,
            self.height as usize,
            radius,
        );
        self.active = self.mask.par_iter().any(|&p| p > 0);
        self.bbox_dirty = true;
        self.mask_revision += 1;
    }

    /// Round the selection edges by `radius` px (Select ▸ Modify ▸ Smooth): blur the
    /// mask then re-threshold at 50% — removes small bumps and fills small notches
    /// symmetrically, so jagged edges become rounded.
    pub fn smooth(&mut self, radius: f32) {
        if radius < 0.5 || !self.active {
            return;
        }
        blur_mask(
            &mut self.mask,
            self.width as usize,
            self.height as usize,
            radius,
        );
        for p in self.mask.iter_mut() {
            *p = if *p >= 128 { 255 } else { 0 };
        }
        self.active = self.mask.par_iter().any(|&p| p > 0);
        self.bbox_dirty = true;
        self.mask_revision += 1;
    }

    /// Replace the selection with a band around its edge (Select ▸ Modify ▸ Border):
    /// the mask grown by `width` px minus the mask shrunk by `width` px (≈ a `2·width`
    /// px band straddling the original edge).
    pub fn border(&mut self, width: u32) {
        if width == 0 || !self.active {
            return;
        }
        let mut outer = self.clone();
        outer.grow(width);
        let mut inner = self.clone();
        inner.shrink(width);
        for (i, p) in self.mask.iter_mut().enumerate() {
            *p = if outer.mask[i] > 0 && inner.mask[i] == 0 {
                255
            } else {
                0
            };
        }
        self.active = self.mask.par_iter().any(|&p| p > 0);
        self.bbox_dirty = true;
        self.mask_revision += 1;
    }

    /// Bake `self.offset` into the mask data, then reset `offset` to (0, 0).
    ///
    /// Call BEFORE boolean ops (Add/Subtract/Intersect) so the current and new masks
    /// share a coordinate space. No need to call manually —
    /// `apply_with_mode()` calls it for non-New modes.
    ///
    /// The caller must wrap it in a SelectionCommand so the committed state
    /// is captured into undo/redo history.
    pub fn commit_offset(&mut self) {
        let (dx, dy) = self.offset;
        if dx == 0 && dy == 0 {
            return;
        }

        let w = self.width as i32;
        let h = self.height as i32;
        let len = (w as usize).checked_mul(h as usize).unwrap_or(0);
        let old_mask = std::mem::replace(&mut self.mask, vec![0u8; len]);

        for y in 0..h {
            for x in 0..w {
                let src_x = x - dx;
                let src_y = y - dy;
                if src_x >= 0 && src_y >= 0 && src_x < w && src_y < h {
                    self.mask[(y * w + x) as usize] = old_mask[(src_y * w + src_x) as usize];
                }
            }
        }

        self.offset = (0, 0);
        self.bbox_dirty = true;
        self.mask_revision += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RefineBrushMode {
    Smart,
    Add,
    Subtract,
}

/// Brush shape falloff: 1.0 at center, 0.0 at/beyond radius.
/// hardness=0.0 → smooth quadratic fade.  hardness=1.0 → nearly hard edge.
#[inline]
fn brush_falloff(dist: f32, radius: f32, hardness: f32) -> f32 {
    let t = (dist / radius).clamp(0.0, 1.0);
    let soft_edge = 1.0 - hardness;
    if soft_edge < 0.01 {
        if t < 1.0 {
            1.0
        } else {
            0.0
        }
    } else {
        let fade_start = 1.0 - soft_edge;
        if t <= fade_start {
            1.0
        } else {
            let fade_t = (t - fade_start) / soft_edge;
            1.0 - fade_t * fade_t
        }
    }
}

/// Plain Refine Brush dab (Add / Subtract) on a selection mask at canvas
/// point (cx, cy); Smart is `core::smart_brush` and does nothing here.
#[allow(clippy::too_many_arguments)]
pub fn refine_edge_stamp(
    mask: &mut [u8],
    width: u32,
    height: u32,
    cx: f32,
    cy: f32,
    radius: f32,
    hardness: f32,
    mode: RefineBrushMode,
) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 {
        return;
    }

    let icx = cx as i32;
    let icy = cy as i32;
    let ir = radius.ceil() as i32;

    match mode {
        RefineBrushMode::Add => {
            for dy in -ir..=ir {
                for dx in -ir..=ir {
                    let px = icx + dx;
                    let py = icy + dy;
                    if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                        continue;
                    }
                    let dist = ((dx * dx + dy * dy) as f32).sqrt();
                    if dist > radius {
                        continue;
                    }
                    let w8 = (brush_falloff(dist, radius, hardness) * 255.0) as u8;
                    let i = py as usize * w + px as usize;
                    mask[i] = mask[i].max(w8);
                }
            }
            return;
        }
        RefineBrushMode::Subtract => {
            for dy in -ir..=ir {
                for dx in -ir..=ir {
                    let px = icx + dx;
                    let py = icy + dy;
                    if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                        continue;
                    }
                    let dist = ((dx * dx + dy * dy) as f32).sqrt();
                    if dist > radius {
                        continue;
                    }
                    let keep = 1.0 - brush_falloff(dist, radius, hardness);
                    let i = py as usize * w + px as usize;
                    mask[i] = (mask[i] as f32 * keep).round() as u8;
                }
            }
            return;
        }
        RefineBrushMode::Smart => {}
    }
}

/// Three passes of a box blur of radius `radius` (≈ Gaussian), windows clipped
/// at the image edge.
pub fn blur_mask(mask: &mut [u8], w: usize, h: usize, radius: f32) {
    if radius < 0.5 {
        return;
    }
    let r = (radius.round() as usize).max(1);
    crate::core::refine::box_blur(mask, w, h, r, 3);
}

#[inline]
fn linearize_srgb(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn f_lab(t: f32) -> f32 {
    if t > 0.008856 {
        t.cbrt()
    } else {
        7.787 * t + 16.0 / 116.0
    }
}

/// sRGB (u8) → CIE L*a*b* (D65 illuminant).
/// L: 0–100, a/b: approx ±128
pub fn rgb_to_lab(r: u8, g: u8, b: u8) -> [f32; 3] {
    let r = linearize_srgb(r);
    let g = linearize_srgb(g);
    let b = linearize_srgb(b);
    let x = 0.4124564 * r + 0.3575761 * g + 0.1804375 * b;
    let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
    let z = 0.0193339 * r + 0.1191920 * g + 0.9503041 * b;
    let fx = f_lab(x / 0.95047);
    let fy = f_lab(y / 1.00000);
    let fz = f_lab(z / 1.08883);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Lab Euclidean distance (ΔE76)
#[inline]
pub fn lab_dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dl = a[0] - b[0];
    let da = a[1] - b[1];
    let db = a[2] - b[2];
    (dl * dl + da * da + db * db).sqrt()
}

/// Convert RGBA pixel buffer → Lab array (alpha ignored)
pub fn pixels_to_lab(pixels: &[u8], width: u32, height: u32) -> Vec<[f32; 3]> {
    let Some(n) = mask_len(width, height) else {
        return Vec::new();
    };
    (0..n)
        .into_par_iter()
        .map(|i| {
            let p = i * 4;
            if p + 2 < pixels.len() {
                rgb_to_lab(pixels[p], pixels[p + 1], pixels[p + 2])
            } else {
                [0.0, 0.0, 0.0]
            }
        })
        .collect()
}

/// Sobel gradient magnitude map.
///
/// The luma edge catches normal light/dark boundaries; the RGB edge catches
/// same-brightness color boundaries, which matter a lot for people/clothing
/// against saturated backgrounds.
pub fn compute_sobel(pixels: &[u8], width: u32, height: u32) -> Vec<f32> {
    let w = width as usize;
    let h = height as usize;
    let n = w * h;
    (0..n)
        .into_par_iter()
        .map(|i| {
            let x = i % w;
            let y = i / w;
            if x == 0 || y == 0 || x + 1 >= w || y + 1 >= h {
                return 0.0f32;
            }

            let sobel_channel = |ch: usize| -> f32 {
                let sample = |sx: usize, sy: usize| -> f32 {
                    pixels.get((sy * w + sx) * 4 + ch).copied().unwrap_or(0) as f32
                };
                let tl = sample(x - 1, y - 1);
                let t = sample(x, y - 1);
                let tr = sample(x + 1, y - 1);
                let ml = sample(x - 1, y);
                let mr = sample(x + 1, y);
                let bl = sample(x - 1, y + 1);
                let bm = sample(x, y + 1);
                let br = sample(x + 1, y + 1);
                let gx = -tl - 2.0 * ml - bl + tr + 2.0 * mr + br;
                let gy = -tl - 2.0 * t - tr + bl + 2.0 * bm + br;
                (gx * gx + gy * gy).sqrt()
            };

            let r = sobel_channel(0);
            let g = sobel_channel(1);
            let b = sobel_channel(2);
            let luma = 0.299 * r + 0.587 * g + 0.114 * b;
            let rgb = (r * r + g * g + b * b).sqrt() * 0.45;
            luma.max(rgb)
        })
        .collect()
}

/// Pre-computed Lab + Sobel data for Quick Select and the Refine Brush.
/// Computed once per layer revision, reused across strokes.
pub struct EdgeCache {
    pub lab: Vec<[f32; 3]>,
    pub sobel: Vec<f32>,
    pub width: u32,
    pub height: u32,
    pub layer_idx: usize,
    pub layer_revision: u64,
    pub sample_merged: bool,
}
