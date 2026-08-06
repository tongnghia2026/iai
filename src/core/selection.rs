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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn box_filter_is_local_mean() {
        let img = vec![1.0f32; 9];
        let out = box_filter(&img, 3, 3, 1);
        assert!((out[4] - 1.0).abs() < 1e-6);
        assert!((out[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn guided_filter_preserves_a_step_edge() {
        let (w, h) = (16usize, 8usize);
        let mut guide = vec![0f32; w * h];
        let mut p = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.0 } else { 1.0 };
                guide[y * w + x] = v;
                p[y * w + x] = v;
            }
        }
        let out = guided_filter(&guide, &p, w, h, 3, 1e-6);
        assert!(out[2] < 0.05, "left flat region drifted: {}", out[2]);
        assert!(
            out[w - 3] > 0.95,
            "right flat region drifted: {}",
            out[w - 3]
        );
        let left_of_edge = out[w / 2 - 1];
        let right_of_edge = out[w / 2];
        assert!(
            right_of_edge - left_of_edge > 0.7,
            "edge was blurred: {left_of_edge} → {right_of_edge}"
        );
    }

    #[test]
    fn guided_filter_smooths_noise_in_a_flat_guide() {
        let (w, h) = (8usize, 8usize);
        let guide = vec![0.5f32; w * h];
        let mut p = vec![0.5f32; w * h];
        p[3 * w + 3] = 1.0;
        let out = guided_filter(&guide, &p, w, h, 2, 1e-4);
        assert!(
            out[3 * w + 3] < 0.7,
            "spike was not smoothed: {}",
            out[3 * w + 3]
        );
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

/// Apply a Refine Brush stamp to the selection mask.
///
/// # Parameters
/// - `lab_cache` — CIE L*a*b* values for every pixel (same layout as mask).
/// - `sobel`     — Sobel gradient magnitude per pixel (from EdgeCache).
/// - `mask`      — selection mask modified in-place.
/// - `cx, cy`    — brush centre in canvas pixel coordinates.
/// - `radius`    — brush radius in pixels.
/// - `hardness`  — 0.0 = feathered, 1.0 = hard edge.
/// - `mode`      — Smart / Add / Subtract.
///
/// # Smart mode algorithm (KNN nearest-sample matting)
/// Uses the *minimum* Lab distance to any known FG / BG sample rather than the
/// mean colour.  This handles multimodal foregrounds (e.g. dark + light hair)
/// where a single average colour collapses the distribution and misclassifies
/// mid-tone pixels as background.
///
/// Samples are collected with a spatial stride so the same number of samples
/// covers the region uniformly regardless of how many definite-FG/BG pixels
/// happen to be near the cursor.
///
/// Edge guidance: pixels far from a colour transition (low Sobel) keep their
/// current mask value; matting only applies where the Sobel exceeds a threshold
/// derived from local FG/BG contrast.
pub fn refine_edge_stamp(
    lab_cache: &[[f32; 3]],
    sobel_map: &[f32],
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

    let sample_r = radius * 2.5;
    let sample_r2 = sample_r * sample_r;
    let isr = sample_r.ceil() as i32;

    const MAX_SAMPLES: usize = 256;
    let sample_area = (std::f64::consts::PI * sample_r as f64 * sample_r as f64) as usize;
    let stride = (((sample_area / MAX_SAMPLES) as f32).sqrt().ceil() as i32).max(1);

    let mut fg_samples: Vec<[f32; 3]> = Vec::with_capacity(MAX_SAMPLES);
    let mut bg_samples: Vec<[f32; 3]> = Vec::with_capacity(MAX_SAMPLES);

    let mut iy = -isr;
    while iy <= isr {
        let mut ix = -isr;
        while ix <= isr {
            let px = icx + ix;
            let py = icy + iy;
            if px >= 0 && py >= 0 && px < w as i32 && py < h as i32 {
                let dsq = (ix * ix + iy * iy) as f32;
                if dsq <= sample_r2 {
                    let i = py as usize * w + px as usize;
                    let m = mask[i];
                    if m >= 220 && fg_samples.len() < MAX_SAMPLES {
                        fg_samples.push(lab_cache[i]);
                    } else if m <= 35 && sobel_map[i] < 140.0 && bg_samples.len() < MAX_SAMPLES {
                        bg_samples.push(lab_cache[i]);
                    }
                }
            }
            ix += stride;
        }
        iy += stride;
    }

    if fg_samples.is_empty() || bg_samples.is_empty() {
        for dy in -isr..=isr {
            for dx in -isr..=isr {
                let px = icx + dx;
                let py = icy + dy;
                if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                    continue;
                }
                if (dx * dx + dy * dy) as f32 > sample_r2 {
                    continue;
                }
                let i = py as usize * w + px as usize;
                let m = mask[i];
                if fg_samples.len() < MAX_SAMPLES && m >= 180 {
                    fg_samples.push(lab_cache[i]);
                }
                if bg_samples.len() < MAX_SAMPLES && m <= 75 && sobel_map[i] < 180.0 {
                    bg_samples.push(lab_cache[i]);
                }
            }
        }
        if fg_samples.is_empty() || bg_samples.is_empty() {
            return;
        }
    }

    let original_fg_samples = fg_samples.clone();
    fg_samples.retain(|&fg| min_lab_dist(&bg_samples, fg) > 8.0);
    if fg_samples.is_empty() {
        fg_samples = original_fg_samples;
    }
    let original_bg_samples = bg_samples.clone();
    bg_samples.retain(|&bg| min_lab_dist(&fg_samples, bg) > 5.0);
    if bg_samples.is_empty() {
        bg_samples = original_bg_samples;
    }

    let mut sobel_sum = 0.0f64;
    let mut sobel_n = 0usize;
    for dy in -ir..=ir {
        for dx in -ir..=ir {
            let px = icx + dx;
            let py = icy + dy;
            if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                continue;
            }
            if (dx * dx + dy * dy) as f32 > radius * radius {
                continue;
            }
            let i = py as usize * w + px as usize;
            sobel_sum += sobel_map[i] as f64;
            sobel_n += 1;
        }
    }
    let edge_thr = if sobel_n > 0 {
        (sobel_sum / sobel_n as f64 * 0.5) as f32
    } else {
        0.0
    };

    let rx0 = (icx - ir).max(0);
    let ry0 = (icy - ir).max(0);
    let rx1 = (icx + ir + 1).min(w as i32);
    let ry1 = (icy + ir + 1).min(h as i32);
    if rx1 <= rx0 || ry1 <= ry0 {
        return;
    }
    let rw = (rx1 - rx0) as usize;
    let rh = (ry1 - ry0) as usize;
    let mut raw = vec![0f32; rw * rh];
    let mut guide = vec![0f32; rw * rh];

    for ry in 0..rh {
        for rx in 0..rw {
            let px = rx0 + rx as i32;
            let py = ry0 + ry as i32;
            let i = py as usize * w + px as usize;
            let lab = lab_cache[i];
            guide[ry * rw + rx] = (lab[0] / 100.0).clamp(0.0, 1.0);
            let cur_alpha = mask[i] as f32 / 255.0;

            let on_edge = sobel_map[i] >= edge_thr;
            let d_fg_min = min_lab_dist(&fg_samples, lab);
            let d_bg_min = min_lab_dist(&bg_samples, lab);
            let bg_wins = d_bg_min + 4.0 < d_fg_min;
            let fg_wins = d_fg_min + 3.0 < d_bg_min;
            let est = if bg_wins && (!on_edge || cur_alpha > 0.01) {
                0.0
            } else if on_edge || fg_wins {
                if d_fg_min + d_bg_min > 0.2 {
                    (d_bg_min / (d_fg_min + d_bg_min)).powf(0.7).clamp(0.0, 1.0)
                } else {
                    cur_alpha
                }
            } else {
                cur_alpha
            };
            raw[ry * rw + rx] = est;
        }
    }

    let gf_r = ((radius * 0.25) as usize).clamp(4, 32);
    let refined = guided_filter(&guide, &raw, rw, rh, gf_r, 1e-4);

    for ry in 0..rh {
        for rx in 0..rw {
            let px = rx0 + rx as i32;
            let py = ry0 + ry as i32;
            let dx = (px - icx) as f32;
            let dy = (py - icy) as f32;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > radius {
                continue;
            }
            let i = py as usize * w + px as usize;
            let cur_alpha = mask[i] as f32 / 255.0;
            let weight = brush_falloff(dist, radius, hardness);
            let lab = lab_cache[i];
            let d_fg_min = min_lab_dist(&fg_samples, lab);
            let d_bg_min = min_lab_dist(&bg_samples, lab);
            let bg_wins = d_bg_min + 4.0 < d_fg_min;
            let fg_wins = d_fg_min + 3.0 < d_bg_min;
            let on_edge = sobel_map[i] >= edge_thr;
            let mut a = refined[ry * rw + rx];
            if bg_wins {
                a = a.min(if on_edge { 0.18 } else { 0.02 });
            } else if fg_wins && (on_edge || cur_alpha < 0.5) {
                a = a.max(0.68);
            }
            let blended = (cur_alpha * (1.0 - weight) + a * weight) * 255.0;
            mask[i] = blended.round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Mean box filter (window `2r+1`) of an f32 image with edge clamping, computed
/// in O(N) via an integral image. Helper for `guided_filter`.
fn box_filter(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let iw = w + 1;
    let mut integ = vec![0f64; iw * (h + 1)];
    for y in 0..h {
        let mut row = 0f64;
        for x in 0..w {
            row += src[y * w + x] as f64;
            integ[(y + 1) * iw + (x + 1)] = integ[y * iw + (x + 1)] + row;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let y0 = y.saturating_sub(r);
        let y1 = (y + r + 1).min(h);
        for x in 0..w {
            let x0 = x.saturating_sub(r);
            let x1 = (x + r + 1).min(w);
            let s = integ[y1 * iw + x1] + integ[y0 * iw + x0]
                - integ[y0 * iw + x1]
                - integ[y1 * iw + x0];
            let cnt = ((x1 - x0) * (y1 - y0)) as f64;
            out[y * w + x] = (s / cnt) as f32;
        }
    }
    out
}

/// Guided filter (He, Sun & Tang 2013) with a single-channel guide. Returns the
/// filtered version of `src` whose edges follow `guide`. `r` is the window radius,
/// `eps` the regularisation (smaller → sharper edge transfer). Both buffers are
/// `w×h`; output is clamped to 0..1.
fn guided_filter(guide: &[f32], src: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let n = w * h;
    if n == 0 || guide.len() < n || src.len() < n {
        return src.to_vec();
    }
    let mean_i = box_filter(guide, w, h, r);
    let mean_p = box_filter(src, w, h, r);
    let ii: Vec<f32> = guide.iter().map(|&v| v * v).collect();
    let ip: Vec<f32> = guide.iter().zip(src).map(|(&i, &p)| i * p).collect();
    let corr_i = box_filter(&ii, w, h, r);
    let corr_ip = box_filter(&ip, w, h, r);

    let mut a = vec![0f32; n];
    let mut b = vec![0f32; n];
    for k in 0..n {
        let var_i = corr_i[k] - mean_i[k] * mean_i[k];
        let cov_ip = corr_ip[k] - mean_i[k] * mean_p[k];
        let ak = cov_ip / (var_i + eps);
        a[k] = ak;
        b[k] = mean_p[k] - ak * mean_i[k];
    }
    let mean_a = box_filter(&a, w, h, r);
    let mean_b = box_filter(&b, w, h, r);
    (0..n)
        .map(|k| (mean_a[k] * guide[k] + mean_b[k]).clamp(0.0, 1.0))
        .collect()
}

pub fn blur_mask(mask: &mut [u8], w: usize, h: usize, radius: f32) {
    if radius < 0.5 {
        return;
    }
    let r = (radius.round() as usize).max(1);

    let mut scratch_row = vec![0u8; w];
    let mut scratch_col = vec![0u8; h];

    for _ in 0..3 {
        for y in 0..h {
            let base = y * w;
            scratch_row.copy_from_slice(&mask[base..base + w]);
            for x in 0..w {
                let x0 = x.saturating_sub(r);
                let x1 = (x + r + 1).min(w);
                let sum: u32 = scratch_row[x0..x1].iter().map(|&v| v as u32).sum();
                mask[base + x] = (sum / (x1 - x0) as u32) as u8;
            }
        }

        for x in 0..w {
            for y in 0..h {
                scratch_col[y] = mask[y * w + x];
            }
            for y in 0..h {
                let y0 = y.saturating_sub(r);
                let y1 = (y + r + 1).min(h);
                let sum: u32 = scratch_col[y0..y1].iter().map(|&v| v as u32).sum();
                mask[y * w + x] = (sum / (y1 - y0) as u32) as u8;
            }
        }
    }
}

/// Adaptive edge-only radius for Refine Selection.
///
/// This builds a band around existing mask transitions and blends in a blurred
/// mask only inside that band. Solid foreground/background stays unchanged, so
/// hard object edges remain crisp while hair/fur gets an adjustable soft region.
pub fn smart_radius_mask(mask: &mut [u8], w: usize, h: usize, radius: f32) {
    if radius < 0.5 || w == 0 || h == 0 || mask.len() < w.saturating_mul(h) {
        return;
    }

    let original = mask.to_vec();
    let mut blurred = original.clone();
    blur_mask(&mut blurred, w, h, radius);

    let mut edge_band = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let v = original[i];
            let mut transition = v > 0 && v < 255;
            if x > 0 && original[i - 1].abs_diff(v) > 8 {
                transition = true;
            }
            if x + 1 < w && original[i + 1].abs_diff(v) > 8 {
                transition = true;
            }
            if y > 0 && original[i - w].abs_diff(v) > 8 {
                transition = true;
            }
            if y + 1 < h && original[i + w].abs_diff(v) > 8 {
                transition = true;
            }
            if transition {
                edge_band[i] = 255;
            }
        }
    }

    blur_mask(&mut edge_band, w, h, radius);
    for i in 0..w * h {
        let band = edge_band[i] as f32 / 255.0;
        if band <= 0.001 {
            continue;
        }
        let weight = band.sqrt();
        let v = original[i] as f32 * (1.0 - weight) + blurred[i] as f32 * weight;
        mask[i] = v.round().clamp(0.0, 255.0) as u8;
    }
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

#[inline]
fn min_lab_dist(samples: &[[f32; 3]], lab: [f32; 3]) -> f32 {
    samples
        .iter()
        .map(|&s| lab_dist(lab, s))
        .fold(f32::MAX, f32::min)
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

/// Pre-computed Lab + Sobel data for Quick Select.
/// Computed once per layer revision, reused across all stamps in the same drag.
pub struct EdgeCache {
    pub lab: Vec<[f32; 3]>,
    pub sobel: Vec<f32>,
    pub width: u32,
    pub height: u32,
    pub layer_idx: usize,
    pub layer_revision: u64,
    pub sample_merged: bool,
}

pub struct SmartSelectStamp {
    pub x0: usize,
    pub y0: usize,
    pub width: usize,
    pub height: usize,
    pub mask: Vec<u8>,
}

/// Quick Select stamp — standard pixel BFS with pre-computed edge map.
///
/// `(cx, cy)` — stamp centre in canvas pixel coordinates
/// `brush_radius` — radius of the seed circle (canvas px)
/// `tolerance` — 0-255 UI → Lab ΔE threshold (32 → ΔE≈15)
/// `edge_sensitivity` — 0=off, 50=portrait, 100=stop at every edge
///
/// Performance: <5ms on 1920×1080 for typical use (colour+edge gates stop
/// the BFS long before it reaches the whole image).
#[cfg(test)]
fn smart_select_stamp_fast(
    cache: &EdgeCache,
    cx: u32,
    cy: u32,
    brush_radius: f32,
    tolerance: u8,
    edge_sensitivity: u8,
) -> Vec<u8> {
    let stamp = smart_select_stamp_region(cache, cx, cy, brush_radius, tolerance, edge_sensitivity);
    let w = cache.width as usize;
    let h = cache.height as usize;
    let n = w * h;
    let mut full = vec![0u8; n];
    for y in 0..stamp.height {
        let src = y * stamp.width;
        let dst = (stamp.y0 + y) * w + stamp.x0;
        full[dst..dst + stamp.width].copy_from_slice(&stamp.mask[src..src + stamp.width]);
    }
    full
}

pub fn smart_select_stamp_region(
    cache: &EdgeCache,
    cx: u32,
    cy: u32,
    brush_radius: f32,
    tolerance: u8,
    edge_sensitivity: u8,
) -> SmartSelectStamp {
    let w = cache.width as usize;
    let h = cache.height as usize;
    let n = w * h;

    if n == 0 {
        return SmartSelectStamp {
            x0: 0,
            y0: 0,
            width: 0,
            height: 0,
            mask: Vec::new(),
        };
    }

    let cx = cx.min(cache.width.saturating_sub(1));
    let cy = cy.min(cache.height.saturating_sub(1));
    let tol_lab: f32 = 8.0 + tolerance as f32 * (58.0 / 255.0);
    let seed_gate = (tol_lab * 0.85).clamp(9.0, 24.0);
    let step_tol = (tol_lab * 0.72 + 5.0).clamp(10.0, 34.0);
    let drift_tol = (tol_lab * 1.85).clamp(18.0, 96.0);

    let edge_thr: f32 = if edge_sensitivity == 0 {
        f32::MAX
    } else {
        (720.0 * (1.0 - edge_sensitivity as f32 / 100.0)).max(24.0)
    };
    let soft_edge_thr = if edge_sensitivity == 0 {
        f32::MAX
    } else {
        edge_thr * 0.72
    };

    let br = brush_radius.ceil() as i32;
    let br2 = brush_radius * brush_radius;
    let icx = cx as i32;
    let icy = cy as i32;
    let center_i = cy as usize * w + cx as usize;
    let center_lab = cache.lab[center_i];
    let grow_radius = (brush_radius * 8.0 + tolerance as f32 * 0.75).clamp(48.0, 640.0);
    let grow2 = grow_radius * grow_radius;
    let roi_pad = 2.0;
    let rx0 = (icx as f32 - grow_radius - roi_pad).floor().max(0.0) as usize;
    let ry0 = (icy as f32 - grow_radius - roi_pad).floor().max(0.0) as usize;
    let rx1 = (icx as f32 + grow_radius + roi_pad).ceil().min(w as f32) as usize;
    let ry1 = (icy as f32 + grow_radius + roi_pad).ceil().min(h as f32) as usize;
    let rw = rx1.saturating_sub(rx0);
    let rh = ry1.saturating_sub(ry0);
    let rn = rw * rh;

    let mut ref_lab = [0.0f32; 3];
    let mut seed_weight = 0.0f32;
    let core_radius = (brush_radius * 0.38).max(2.5);
    let core2 = core_radius * core_radius;
    let mut seed_pixels: Vec<usize> =
        Vec::with_capacity(((std::f32::consts::PI * brush_radius * brush_radius) as usize).min(rn));

    let mut visited = vec![false; rn];
    let cap = ((std::f32::consts::PI * brush_radius * brush_radius) as usize + 64).min(rn);
    let mut queue: std::collections::VecDeque<u32> =
        std::collections::VecDeque::with_capacity(cap.min(1 << 20));

    for dy in -br..=br {
        for dx in -br..=br {
            if (dx * dx + dy * dy) as f32 > br2 {
                continue;
            }
            let px = icx + dx;
            let py = icy + dy;
            if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                continue;
            }
            let pi = py as usize * w + px as usize;
            let li = (py as usize - ry0) * rw + (px as usize - rx0);
            let lab = cache.lab[pi];
            let d2 = (dx * dx + dy * dy) as f32;
            let near_center = d2 <= core2;
            let center_dist = lab_dist(center_lab, lab);
            let color_ok = center_dist <= seed_gate;
            let core_ok = near_center && center_dist <= seed_gate * 1.25;
            if core_ok || color_ok {
                let radial = 1.0 - (d2 / br2.max(1.0)).sqrt();
                let edge_weight = 1.0 - (cache.sobel[pi] / edge_thr.max(1.0)).clamp(0.0, 0.85);
                let weight = (0.25 + radial.max(0.0)) * edge_weight;
                ref_lab[0] += lab[0] * weight;
                ref_lab[1] += lab[1] * weight;
                ref_lab[2] += lab[2] * weight;
                seed_weight += weight;
                seed_pixels.push(li);
            }
        }
    }

    if seed_pixels.is_empty() {
        seed_pixels.push((cy as usize - ry0) * rw + (cx as usize - rx0));
        ref_lab = center_lab;
        seed_weight = 1.0;
    }

    ref_lab = [
        ref_lab[0] / seed_weight,
        ref_lab[1] / seed_weight,
        ref_lab[2] / seed_weight,
    ];

    let mut mask = vec![0u8; rn];
    for &li in &seed_pixels {
        if !visited[li] {
            visited[li] = true;
            queue.push_back(li as u32);
        }
        mask[li] = 255;
    }

    while let Some(li) = queue.pop_front() {
        let li = li as usize;
        let lx = li % rw;
        let ly = li / rw;
        let px = (rx0 + lx) as i32;
        let py = (ry0 + ly) as i32;
        let pi = py as usize * w + px as usize;

        const DIRS: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
        for (ndx, ndy) in DIRS {
            let nx = px + ndx;
            let ny = py + ndy;
            if nx < rx0 as i32 || ny < ry0 as i32 || nx >= rx1 as i32 || ny >= ry1 as i32 {
                continue;
            }
            let ni = ny as usize * w + nx as usize;
            let nli = (ny as usize - ry0) * rw + (nx as usize - rx0);
            if visited[nli] {
                continue;
            }

            let from_center_x = nx - icx;
            let from_center_y = ny - icy;
            if (from_center_x * from_center_x + from_center_y * from_center_y) as f32 > grow2 {
                continue;
            }

            let candidate_lab = cache.lab[ni];
            let ref_dist = lab_dist(ref_lab, candidate_lab);
            let step_dist = lab_dist(cache.lab[pi], candidate_lab);
            let edge = cache.sobel[ni].max(cache.sobel[pi]);

            if edge > edge_thr && step_dist > step_tol * 0.45 {
                continue;
            }

            let direct_match = ref_dist <= tol_lab;
            let local_match =
                step_dist <= step_tol && ref_dist <= drift_tol && edge <= soft_edge_thr;
            if !direct_match && !local_match {
                continue;
            }

            visited[nli] = true;
            mask[nli] = 255;
            queue.push_back(nli as u32);
        }
    }

    let max_hole_area = (brush_radius * brush_radius * 2.4)
        .round()
        .clamp(80.0, 18_000.0) as usize;
    let max_hole_span = (brush_radius * 2.3).round().clamp(10.0, 220.0) as usize;
    fill_small_subject_holes(&mut mask, rw, 0, 0, rw, rh, max_hole_area, max_hole_span);

    SmartSelectStamp {
        x0: rx0,
        y0: ry0,
        width: rw,
        height: rh,
        mask,
    }
}

fn fill_small_subject_holes(
    mask: &mut [u8],
    w: usize,
    rx0: usize,
    ry0: usize,
    rx1: usize,
    ry1: usize,
    max_area: usize,
    max_span: usize,
) {
    if rx1 <= rx0 || ry1 <= ry0 {
        return;
    }

    let rw = rx1 - rx0;
    let rh = ry1 - ry0;
    let mut seen = vec![false; rw * rh];
    let mut queue = std::collections::VecDeque::new();
    let mut component = Vec::new();

    for sy in 0..rh {
        for sx in 0..rw {
            let si = sy * rw + sx;
            let gi = (ry0 + sy) * w + (rx0 + sx);
            if seen[si] || mask.get(gi).copied().unwrap_or(255) > 0 {
                continue;
            }

            seen[si] = true;
            queue.clear();
            component.clear();
            queue.push_back((sx, sy));
            let mut touches_border = false;
            let mut min_x = sx;
            let mut min_y = sy;
            let mut max_x = sx;
            let mut max_y = sy;

            while let Some((x, y)) = queue.pop_front() {
                component.push((x, y));
                touches_border |= x == 0 || y == 0 || x + 1 == rw || y + 1 == rh;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);

                for (nx, ny) in [
                    (x.wrapping_sub(1), y),
                    (x + 1, y),
                    (x, y.wrapping_sub(1)),
                    (x, y + 1),
                ] {
                    if nx >= rw || ny >= rh {
                        continue;
                    }
                    let ni = ny * rw + nx;
                    let gi = (ry0 + ny) * w + (rx0 + nx);
                    if seen[ni] || mask.get(gi).copied().unwrap_or(255) > 0 {
                        continue;
                    }
                    seen[ni] = true;
                    queue.push_back((nx, ny));
                }
            }

            let span_x = max_x - min_x + 1;
            let span_y = max_y - min_y + 1;
            if !touches_border
                && component.len() <= max_area
                && span_x <= max_span
                && span_y <= max_span
            {
                for (x, y) in component.iter().copied() {
                    mask[(ry0 + y) * w + (rx0 + x)] = 255;
                }
            }
        }
    }
}

#[cfg(test)]
mod smart_select_tests {
    use super::*;

    fn cache_from_pixels(pixels: Vec<u8>, width: u32, height: u32) -> EdgeCache {
        EdgeCache {
            lab: pixels_to_lab(&pixels, width, height),
            sobel: compute_sobel(&pixels, width, height),
            width,
            height,
            layer_idx: 0,
            layer_revision: 0,
            sample_merged: true,
        }
    }

    #[test]
    fn smart_select_rejects_background_inside_large_brush() {
        let w = 80u32;
        let h = 50u32;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let inside_subject = (20..=60).contains(&x) && (10..=40).contains(&y);
                let rgb = if inside_subject {
                    [210, 40, 35]
                } else {
                    [30, 90, 210]
                };
                pixels[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }

        let cache = cache_from_pixels(pixels, w, h);
        let mask = smart_select_stamp_fast(&cache, 23, 25, 14.0, 32, 65);

        assert_eq!(mask[(25 * w + 40) as usize], 255);
        assert_eq!(mask[(25 * w + 5) as usize], 0);
    }

    #[test]
    fn smart_select_stops_at_same_luma_color_edge() {
        let w = 60u32;
        let h = 36u32;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let rgb = if x < 30 { [180, 50, 50] } else { [50, 115, 50] };
                pixels[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }

        let cache = cache_from_pixels(pixels, w, h);
        let mask = smart_select_stamp_fast(&cache, 18, 18, 12.0, 48, 70);

        assert_eq!(mask[(18 * w + 18) as usize], 255);
        assert_eq!(mask[(18 * w + 42) as usize], 0);
    }

    #[test]
    fn smart_select_fills_small_internal_face_features() {
        let w = 96u32;
        let h = 72u32;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let face = (24..=72).contains(&x) && (10..=62).contains(&y);
                let eye =
                    ((34..=42).contains(&x) || (54..=62).contains(&x)) && (28..=34).contains(&y);
                let mouth = (40..=56).contains(&x) && (48..=53).contains(&y);
                let rgb = if eye || mouth {
                    [35, 22, 18]
                } else if face {
                    [190, 118, 82]
                } else {
                    [35, 95, 210]
                };
                pixels[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }

        let cache = cache_from_pixels(pixels, w, h);
        let mask = smart_select_stamp_fast(&cache, 48, 36, 22.0, 32, 65);

        assert_eq!(mask[(31 * w + 38) as usize], 255);
        assert_eq!(mask[(50 * w + 48) as usize], 255);
        assert_eq!(mask[(36 * w + 8) as usize], 0);
    }

    #[test]
    fn smart_select_does_not_seed_white_background_next_to_dark_hair() {
        let w = 96u32;
        let h = 72u32;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let hair = (34..=58).contains(&x) && (5..=66).contains(&y);
                let skin = x < 34 && (8..=66).contains(&y);
                let rgb = if hair {
                    [18, 17, 20]
                } else if skin {
                    [218, 156, 132]
                } else {
                    [248, 248, 246]
                };
                pixels[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }

        let cache = cache_from_pixels(pixels, w, h);
        let mask = smart_select_stamp_fast(&cache, 54, 34, 18.0, 32, 65);

        assert_eq!(mask[(34 * w + 48) as usize], 255);
        assert_eq!(mask[(34 * w + 72) as usize], 0);
    }

    #[test]
    fn refine_edge_keeps_dark_hair_and_removes_white_background_spill() {
        let w = 96u32;
        let h = 72u32;
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let hair_mass = (28..=44).contains(&x) && (8..=64).contains(&y);
                let hair_strand = (56..=57).contains(&x) && (12..=60).contains(&y);
                let rgb = if hair_mass || hair_strand {
                    [18, 17, 20]
                } else {
                    [248, 248, 246]
                };
                pixels[i..i + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }

        let lab = pixels_to_lab(&pixels, w, h);
        let sobel = compute_sobel(&pixels, w, h);
        let mut mask = vec![0u8; (w * h) as usize];
        for y in 8..=64 {
            for x in 28..=50 {
                mask[(y * w + x) as usize] = 255;
            }
        }

        refine_edge_stamp(
            &lab,
            &sobel,
            &mut mask,
            w,
            h,
            52.0,
            36.0,
            20.0,
            0.7,
            RefineBrushMode::Smart,
        );

        assert!(
            mask[(36 * w + 57) as usize] > 150,
            "dark hair strand should be retained"
        );
        assert!(
            mask[(36 * w + 49) as usize] < 80,
            "white background spill should be removed"
        );
    }
}
