// Smart Fill — synthesise plausible pixels inside a "hole" (the selected
// region) by borrowing texture from the rest of the image. This is a from-scratch
// CPU implementation of multi-scale PatchMatch inpainting (Barnes et al. 2009,
// "PatchMatch", + the coarse-to-fine EM reconstruction used by standard
// content-aware fill).
//
// Pipeline (per call):
//   1. Crop to the hole's bounding box, expanded by a search band so we keep
//      enough surrounding source texture (and stay fast on huge layers).
//   2. Optionally downscale that work region to a pixel budget so a giant fill
//      can't peg the CPU for seconds.
//   3. Build a Gaussian-ish pyramid down to a coarse level.
//   4. Coarse→fine: at each level run a few EM iterations of
//        a) PatchMatch nearest-neighbour search  (hole patch → best source patch)
//        b) vote/reconstruct                      (rebuild hole from source patches)
//      seeding each finer level from the coarser solution.
//   5. Upsample the solved hole back to full resolution and write ONLY hole
//      pixels into the caller's buffer (source pixels are never touched).
//
// All heavy loops are bounded; the vote pass is parallel (rayon). The PatchMatch
// search is inherently sequential (scan-line propagation) so it runs on one
// thread but only ever visits hole pixels, which is cheap after the budget cap.

use rayon::prelude::*;

const HARMONIC_MIN_ITERS: usize = 12;

/// Tuning knobs. Defaults are a good balance of quality vs. speed for photos.
#[derive(Debug, Clone, Copy)]
pub struct CAParams {
    /// Patch edge length in pixels (forced odd). Larger = smoother, slower.
    pub patch: usize,
    /// EM iterations (search + vote) per pyramid level.
    pub iters: usize,
    /// Maximum pyramid levels (coarsest is capped by this and by min size).
    pub max_levels: usize,
    /// Pixel budget for the finest working level. Above this the work region is
    /// downscaled so the fill stays interactive.
    pub budget: usize,
}

impl Default for CAParams {
    fn default() -> Self {
        Self {
            patch: 7,
            iters: 6,
            max_levels: 6,
            budget: 1_400_000,
        }
    }
}

/// Fill `hole` pixels of a straight-alpha RGBA8 buffer (`w×h`) with synthesised
/// content. `hole[y*w+x] == true` marks a pixel to replace. Returns `true` if any
/// pixel was changed. Source (non-hole) pixels are left exactly as they were.
pub fn fill(rgba: &mut [u8], w: usize, h: usize, hole: &[bool]) -> bool {
    fill_with(rgba, w, h, hole, &CAParams::default())
}

pub fn fill_with(rgba: &mut [u8], w: usize, h: usize, hole: &[bool], params: &CAParams) -> bool {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 || hole.len() < w * h {
        return false;
    }
    let patch = (params.patch | 1).max(3);
    let Some((hx0, hy0, hx1, hy1)) = hole_bbox(hole, w, h) else {
        return false;
    };

    let hole_w = hx1 - hx0;
    let hole_h = hy1 - hy0;
    let band = (hole_w.max(hole_h) / 2 + 4 * patch).clamp(2 * patch + 4, 400);
    let rx0 = hx0.saturating_sub(band);
    let ry0 = hy0.saturating_sub(band);
    let rx1 = (hx1 + band).min(w);
    let ry1 = (hy1 + band).min(h);
    let rw = rx1 - rx0;
    let rh = ry1 - ry0;

    let mut region = vec![0f32; rw * rh * 4];
    let mut region_hole = vec![false; rw * rh];
    let mut source_count = 0usize;
    for y in 0..rh {
        for x in 0..rw {
            let si = ((ry0 + y) * w + (rx0 + x)) * 4;
            let di = (y * rw + x) * 4;
            region[di] = rgba[si] as f32;
            region[di + 1] = rgba[si + 1] as f32;
            region[di + 2] = rgba[si + 2] as f32;
            region[di + 3] = rgba[si + 3] as f32;
            let hidx = (ry0 + y) * w + (rx0 + x);
            if hole[hidx] {
                region_hole[y * rw + x] = true;
            } else {
                source_count += 1;
            }
        }
    }
    if source_count < patch * patch {
        return false;
    }

    let mut scale = 1usize;
    while (rw / (scale + 1)).max(1) * (rh / (scale + 1)).max(1) > params.budget {
        scale += 1;
    }
    while scale > 1 && ((rw / scale).max(1) < 4 * patch || (rh / scale).max(1) < 4 * patch) {
        scale -= 1;
    }

    let base = if scale > 1 {
        downscale_level(&region, &region_hole, rw, rh, scale, patch)
    } else {
        Level::new(region, region_hole.clone(), rw, rh, patch)
    };

    let mut levels: Vec<Level> = vec![base];
    while levels.len() < params.max_levels {
        let top = levels.last().unwrap();
        if top.w / 2 < 4 * patch || top.h / 2 < 4 * patch {
            break;
        }
        let coarser = downscale_level(&top.color, &top.hole, top.w, top.h, 2, patch);
        levels.push(coarser);
    }

    let last = levels.len() - 1;
    let mut rng = Rng::new(0x9E3779B9);
    for li in (0..levels.len()).rev() {
        if li == last {
            levels[li].init_coarsest();
        } else {
            let (coarse_color, coarse_nnf, cw, ch) = {
                let c = &levels[li + 1];
                (c.color.clone(), c.nnf.clone(), c.w, c.h)
            };
            levels[li].seed_from_coarser(&coarse_color, &coarse_nnf, cw, ch);
        }
        levels[li].solve(params.iters, &mut rng);
    }
    // Finish with a few search + copy rounds: once the hole carries real
    // texture, the search stops preferring bland patches that merely match a
    // smoothed estimate.
    for it in 0..3 {
        levels[0].patchmatch_pass_biased(params.iters + it, &mut rng, 0.8);
        levels[0].take_best_match_pixels();
    }

    let finest = &levels[0];
    if scale > 1 {
        for y in 0..rh {
            let fy = (y / scale).min(finest.h - 1);
            for x in 0..rw {
                if !region_hole[y * rw + x] {
                    continue;
                }
                let fx = (x / scale).min(finest.w - 1);
                let fi = (fy * finest.w + fx) * 4;
                let si = ((ry0 + y) * w + (rx0 + x)) * 4;
                for c in 0..4 {
                    rgba[si + c] = finest.color[fi + c].round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    } else {
        for y in 0..rh {
            for x in 0..rw {
                if !region_hole[y * rw + x] {
                    continue;
                }
                let fi = (y * finest.w + x) * 4;
                let si = ((ry0 + y) * w + (rx0 + x)) * 4;
                for c in 0..4 {
                    rgba[si + c] = finest.color[fi + c].round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    true
}

/// Rebuild the `hole` of an already-filled buffer at full resolution from the
/// photo's own patches, guided by the fill already there (e.g. an AI result
/// made at a lower resolution): the structure follows the guide, the grain is
/// the photo's. Returns false (buffer untouched) when there is nothing to do or
/// the work region is too large to refine at full resolution.
pub fn refine_fill(rgba: &mut [u8], w: usize, h: usize, hole: &[bool]) -> bool {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 || hole.len() < w * h {
        return false;
    }
    let params = CAParams::default();
    let patch = (params.patch | 1).max(3);
    let Some((hx0, hy0, hx1, hy1)) = hole_bbox(hole, w, h) else {
        return false;
    };
    let band = ((hx1 - hx0).max(hy1 - hy0) / 2 + 4 * patch).clamp(2 * patch + 4, 400);
    let rx0 = hx0.saturating_sub(band);
    let ry0 = hy0.saturating_sub(band);
    let rx1 = (hx1 + band).min(w);
    let ry1 = (hy1 + band).min(h);
    let (rw, rh) = (rx1 - rx0, ry1 - ry0);
    if rw * rh > params.budget {
        return false;
    }
    let mut region = vec![0f32; rw * rh * 4];
    let mut region_hole = vec![false; rw * rh];
    for y in 0..rh {
        for x in 0..rw {
            let si = ((ry0 + y) * w + (rx0 + x)) * 4;
            for c in 0..4 {
                region[(y * rw + x) * 4 + c] = rgba[si + c] as f32;
            }
            region_hole[y * rw + x] = hole[(ry0 + y) * w + (rx0 + x)];
        }
    }
    let mut level = Level::new(region, region_hole, rw, rh, patch);
    if level.valid_centers.is_empty() || level.holes.is_empty() {
        return false;
    }
    level.random_nnf();
    let mut rng = Rng::new(0x51F1_ED00);
    // Match against the guide first, then settle into coherent real copies.
    for it in 0..4 {
        let coherence = if it == 0 { 1.0 } else { 0.8 };
        level.patchmatch_pass_biased(it, &mut rng, coherence);
        level.take_best_match_pixels();
    }
    for y in 0..rh {
        for x in 0..rw {
            if !level.hole[y * rw + x] {
                continue;
            }
            let fi = (y * rw + x) * 4;
            let si = ((ry0 + y) * w + (rx0 + x)) * 4;
            for c in 0..4 {
                rgba[si + c] = level.color[fi + c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    true
}

/// Relax an RGB correction field inside `active` until it becomes harmonic.
/// `fixed` pixels are Dirichlet anchors; free active pixels are solved by a
/// red/black SOR pass. `value_scale` is 1.0 for normalized colors and 255.0 for
/// byte-domain colors, and is used only for the convergence tolerance.
pub(crate) fn solve_harmonic_rgb(
    corr: &mut [f32],
    active: &[bool],
    fixed: &[bool],
    w: usize,
    h: usize,
    value_scale: f32,
) {
    let n = w.saturating_mul(h);
    if w == 0 || h == 0 || corr.len() < n * 3 || active.len() < n || fixed.len() < n {
        return;
    }

    let mut free = [Vec::new(), Vec::new()];
    let mut free_count = 0usize;
    let mut fixed_count = 0usize;
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let i = row + x;
            if !active[i] {
                continue;
            }
            if fixed[i] {
                fixed_count += 1;
            } else {
                free[(x + y) & 1].push(i);
                free_count += 1;
            }
        }
    }
    if free_count == 0 || fixed_count == 0 {
        return;
    }

    // Conservative SOR: small regions converge quickly; large regions get a
    // little more relaxation without becoming twitchy around thin masks.
    let span = (free_count as f32).sqrt();
    let omega = (1.18 + 0.62 * (span / (span + 42.0))).clamp(1.18, 1.78);
    let max_iter = if free_count > 200_000 {
        90
    } else if free_count > 50_000 {
        160
    } else {
        360
    };
    let tol = (0.08 * value_scale / 255.0).max(0.000_01);
    let tol2 = tol * tol * free_count as f32 * 3.0;

    for iter in 0..max_iter {
        let mut err = 0.0f32;
        for parity in 0..2 {
            for &i in &free[parity] {
                let x = i % w;
                let y = i / w;
                let mut sum = [0.0f32; 3];
                let mut count = 0.0f32;

                if x > 0 && active[i - 1] {
                    let b = (i - 1) * 3;
                    sum[0] += corr[b];
                    sum[1] += corr[b + 1];
                    sum[2] += corr[b + 2];
                    count += 1.0;
                }
                if x + 1 < w && active[i + 1] {
                    let b = (i + 1) * 3;
                    sum[0] += corr[b];
                    sum[1] += corr[b + 1];
                    sum[2] += corr[b + 2];
                    count += 1.0;
                }
                if y > 0 && active[i - w] {
                    let b = (i - w) * 3;
                    sum[0] += corr[b];
                    sum[1] += corr[b + 1];
                    sum[2] += corr[b + 2];
                    count += 1.0;
                }
                if y + 1 < h && active[i + w] {
                    let b = (i + w) * 3;
                    sum[0] += corr[b];
                    sum[1] += corr[b + 1];
                    sum[2] += corr[b + 2];
                    count += 1.0;
                }
                if count <= 0.0 {
                    continue;
                }

                let base = i * 3;
                for ch in 0..3 {
                    let target = sum[ch] / count;
                    let old = corr[base + ch];
                    let next = old + (target - old) * omega;
                    let delta = next - old;
                    corr[base + ch] = next;
                    err += delta * delta;
                }
            }
        }
        if iter >= HARMONIC_MIN_ITERS && err <= tol2 {
            break;
        }
    }
}

/// Like [`fill`], but afterwards harmonically blends the synthesised hole into
/// its surroundings (Poisson membrane). Use this for the Repair Brush's
/// content-aware mode: it removes the hard mask seam and bends the fill's colour
/// to meet the neighbours (no more "wrong-colour brush stroke") while keeping the
/// synthesised texture's detail. Returns true if anything was filled.
pub fn fill_seamless(rgba: &mut [u8], w: usize, h: usize, hole: &[bool]) -> bool {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 || hole.len() < w * h {
        return false;
    }
    let orig = rgba.to_vec();
    if !fill(rgba, w, h, hole) {
        return false;
    }
    seamless_blend(&orig, rgba, w, h, hole);
    true
}

/// Tip coverage at or above which a Smart Repair stroke replaces a pixel —
/// the painted footprint its on-canvas overlay shows.
pub const SPOT_HEAL_COVER: f32 = 0.1;
/// Painted footprint (px) from which a Smart Repair stroke is rebuilt by the
/// AI inpainter when its model is available: whole objects rather than spots.
pub const SPOT_HEAL_AI_MIN_AREA: usize = 10_000;
/// Ring (px) healed around the painted footprint, so the soft halo around a
/// blemish or object does not become the fill's boundary colour.
const SPOT_HEAL_GROW: usize = 2;

/// The Repair Brush's Smart (content-aware) heal, like a spot-healing brush:
/// the whole painted footprint (`cover` ≥ [`SPOT_HEAL_COVER`], grown by a
/// small ring) is synthesised from its surroundings with PatchMatch and
/// Poisson-blended in. The tip's softness only shapes the footprint — it no
/// longer fades the fill, which used to leave half of a blemish showing near
/// the stroke edge. `opacity` mixes the result with the original. Returns
/// `true` if anything changed.
pub fn fill_spot(rgba: &mut [u8], w: usize, h: usize, cover: &[f32], opacity: f32) -> bool {
    fill_spot_with(rgba, w, h, cover, opacity, false)
}

/// [`fill_spot`], optionally letting the LaMa model rebuild the footprint
/// first (large areas: whole objects), then restoring the photo's own texture
/// on top at full resolution ([`refine_fill`]) — LaMa works at 512 px and
/// smooths grain. Falls back to PatchMatch when the model is missing or
/// fails. Takes seconds on a CPU with AI: run it off the UI thread.
pub fn fill_spot_with(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    cover: &[f32],
    opacity: f32,
    use_ai: bool,
) -> bool {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 || cover.len() < w * h {
        return false;
    }
    let mut hole: Vec<bool> = cover[..w * h]
        .iter()
        .map(|&v| v >= SPOT_HEAL_COVER)
        .collect();
    grow_mask(&mut hole, w, h, SPOT_HEAL_GROW);
    if !hole.iter().any(|&b| b) {
        return false;
    }
    let orig = rgba.to_vec();
    let by_ai = use_ai && crate::core::lama::inpaint(rgba, w, h, &hole);
    if by_ai {
        refine_fill(rgba, w, h, &hole);
    } else if !fill(rgba, w, h, &hole) {
        return false;
    }
    seamless_blend(&orig, rgba, w, h, &hole);

    let op = opacity.clamp(0.0, 1.0);
    let mut changed = false;
    for i in 0..w * h {
        if !hole[i] {
            continue;
        }
        for c in 0..3 {
            let o = orig[i * 4 + c] as f32;
            let s = rgba[i * 4 + c] as f32;
            let v = (o + (s - o) * op).round().clamp(0.0, 255.0) as u8;
            changed |= v != orig[i * 4 + c];
            rgba[i * 4 + c] = v;
        }
    }
    changed
}

/// Dilate a mask by `r` pixels (8-neighbourhood, `r` passes).
fn grow_mask(mask: &mut [bool], w: usize, h: usize, r: usize) {
    for _ in 0..r {
        let src = mask.to_vec();
        for y in 0..h {
            for x in 0..w {
                if src[y * w + x] {
                    continue;
                }
                let hit = (y.saturating_sub(1)..(y + 2).min(h))
                    .any(|ny| (x.saturating_sub(1)..(x + 2).min(w)).any(|nx| src[ny * w + nx]));
                if hit {
                    mask[y * w + x] = true;
                }
            }
        }
    }
}

/// Seamless-clone a source region into a destination region of the SAME image —
/// the Patch tool's Normal / Source mode (Pérez et al. 2003 "Poisson Image
/// Editing"). `mask` (f32 0..1, len `w*h`) marks the DESTINATION pixels; for each
/// such pixel `(x,y)` the source is taken from `(x+dx, y+dy)` in a snapshot of
/// `rgba` (so overlapping source/dest can't feed back). The source *texture* is
/// transplanted but its colour/brightness is bent to meet the destination's
/// surroundings exactly on the region rim (Laplace membrane), then feathered back
/// through `mask` so a soft mask edge leaves no seam. Returns true if anything
/// changed. Mirrors `seamless_blend`'s solver but reads the source at a fixed
/// offset instead of synthesising it.
pub fn seamless_clone_region(
    rgba: &mut [u8],
    w: usize,
    h: usize,
    mask: &[f32],
    dx: i32,
    dy: i32,
) -> bool {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 || mask.len() < w * h {
        return false;
    }
    const CORE: f32 = 0.04;
    let region: Vec<bool> = mask.iter().map(|&v| v >= CORE).collect();
    let idxs: Vec<usize> = (0..w * h).filter(|&i| region[i]).collect();
    if idxs.is_empty() {
        return false;
    }
    let orig = rgba.to_vec();
    let n = w * h;

    // Source RGB (0..255) for destination index `i`, sampled at the drag offset.
    let src_at = |i: usize| -> Option<[f32; 3]> {
        let x = (i % w) as i32 + dx;
        let y = (i / w) as i32 + dy;
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            return None;
        }
        let si = (y as usize * w + x as usize) * 4;
        Some([orig[si] as f32, orig[si + 1] as f32, orig[si + 2] as f32])
    };

    let mut corr = vec![0f32; n * 3];
    let mut fixed = vec![false; n];
    let mut has_src = vec![false; n];

    // Rim pixels (region pixels touching a non-region pixel or the image edge) are
    // pinned so out == original destination → the patch meets its surroundings with
    // no seam; corr = dst_orig − src there.
    for &i in &idxs {
        let Some(s) = src_at(i) else {
            continue;
        };
        has_src[i] = true;
        let x = i % w;
        let y = i / w;
        let is_rim = x == 0
            || y == 0
            || x + 1 >= w
            || y + 1 >= h
            || !region[i - 1]
            || !region[i + 1]
            || !region[i - w]
            || !region[i + w];
        if is_rim {
            for c in 0..3 {
                corr[i * 3 + c] = orig[i * 4 + c] as f32 - s[c];
            }
            fixed[i] = true;
        }
    }

    solve_harmonic_rgb(&mut corr, &has_src, &fixed, w, h, 255.0);

    let mut changed = false;
    for &i in &idxs {
        let Some(s) = src_at(i) else {
            continue;
        };
        let m = mask[i].clamp(0.0, 1.0);
        for c in 0..3 {
            let healed = (s[c] + corr[i * 3 + c]).clamp(0.0, 255.0);
            let o = orig[i * 4 + c] as f32;
            let v = (o * (1.0 - m) + healed * m).round().clamp(0.0, 255.0) as u8;
            if v != rgba[i * 4 + c] {
                changed = true;
            }
            rgba[i * 4 + c] = v;
        }
    }
    changed
}

#[inline]
fn src_rgb(orig: &[u8], hole: &[bool], ni: usize) -> Option<[f32; 3]> {
    if hole[ni] {
        None
    } else {
        Some([
            orig[ni * 4] as f32,
            orig[ni * 4 + 1] as f32,
            orig[ni * 4 + 2] as f32,
        ])
    }
}

/// Harmonically relax a per-pixel RGB correction so the synthesised hole meets the
/// surrounding source pixels with no seam. `orig` = pixels before fill, `rgba` =
/// after fill (hole already synthesised); the corrected result is written to
/// `rgba`. Rim hole pixels (those touching a source pixel) are pinned so the fill
/// matches the neighbours exactly; interior corrections diffuse inward (Laplace),
/// shifting only low-frequency colour and preserving the synthesised texture.
fn seamless_blend(orig: &[u8], rgba: &mut [u8], w: usize, h: usize, hole: &[bool]) {
    let idxs: Vec<usize> = (0..w * h).filter(|&i| hole[i]).collect();
    if idxs.is_empty() {
        return;
    }
    let n = w * h;
    let mut corr = vec![0f32; n * 3];
    let mut fixed = vec![false; n];

    for &i in &idxs {
        let x = i % w;
        let y = i / w;
        let neigh = [
            (x > 0).then(|| i - 1),
            (x + 1 < w).then(|| i + 1),
            (y > 0).then(|| i - w),
            (y + 1 < h).then(|| i + w),
        ];
        let mut sum = [0f32; 3];
        let mut cnt = 0f32;
        for ni in neigh.into_iter().flatten() {
            if let Some(s) = src_rgb(orig, hole, ni) {
                sum[0] += s[0];
                sum[1] += s[1];
                sum[2] += s[2];
                cnt += 1.0;
            }
        }
        if cnt > 0.0 {
            for c in 0..3 {
                corr[i * 3 + c] = sum[c] / cnt - rgba[i * 4 + c] as f32;
            }
            fixed[i] = true;
        }
    }

    solve_harmonic_rgb(&mut corr, hole, &fixed, w, h, 255.0);

    for &i in &idxs {
        for c in 0..3 {
            let v = rgba[i * 4 + c] as f32 + corr[i * 3 + c];
            rgba[i * 4 + c] = v.clamp(0.0, 255.0) as u8;
        }
        rgba[i * 4 + 3] = 255;
    }
}

struct Level {
    w: usize,
    h: usize,
    hp: usize,
    /// RGBA f32 (0..255). Non-hole pixels are fixed source; hole pixels evolve.
    color: Vec<f32>,
    hole: Vec<bool>,
    /// `valid[i]` — a patch centred here is fully in-bounds and contains no hole
    /// pixel, i.e. it is a usable *source* patch.
    valid: Vec<bool>,
    valid_centers: Vec<(i32, i32)>,
    holes: Vec<(i32, i32)>,
    /// For each hole pixel: absolute source-centre it currently maps to.
    nnf: Vec<(i32, i32)>,
    /// Patch distance of each hole pixel's current match (weights its vote).
    dist: Vec<f32>,
}

impl Level {
    fn new(color: Vec<f32>, hole: Vec<bool>, w: usize, h: usize, patch: usize) -> Self {
        let hp = patch / 2;
        let valid = compute_valid(&hole, w, h, hp);
        let mut valid_centers = Vec::new();
        let mut holes = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if valid[i] {
                    valid_centers.push((x as i32, y as i32));
                }
                if hole[i] {
                    holes.push((x as i32, y as i32));
                }
            }
        }
        let nnf = vec![(0i32, 0i32); w * h];
        let dist = vec![0f32; w * h];
        Self {
            w,
            h,
            hp,
            color,
            hole,
            valid,
            valid_centers,
            holes,
            nnf,
            dist,
        }
    }

    #[inline]
    fn valid_at(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return false;
        }
        self.valid[y as usize * self.w + x as usize]
    }

    /// Average colour of source pixels — the fallback / coarsest seed.
    fn source_average(&self) -> [f32; 4] {
        let mut acc = [0f64; 4];
        let mut n = 0u64;
        for y in 0..self.h {
            for x in 0..self.w {
                let i = y * self.w + x;
                if self.hole[i] {
                    continue;
                }
                let p = i * 4;
                for c in 0..4 {
                    acc[c] += self.color[p + c] as f64;
                }
                n += 1;
            }
        }
        if n == 0 {
            return [128.0, 128.0, 128.0, 255.0];
        }
        [
            (acc[0] / n as f64) as f32,
            (acc[1] / n as f64) as f32,
            (acc[2] / n as f64) as f32,
            (acc[3] / n as f64) as f32,
        ]
    }

    /// Seed the coarsest level: a smooth (harmonic) interpolation of the colours
    /// around the hole, so each part of the hole starts near the content that
    /// borders it (sky above, grass below) instead of one flat average that
    /// steers the search toward bland patches. Random NNF.
    fn init_coarsest(&mut self) {
        let avg = self.source_average();
        let (w, h) = (self.w, self.h);
        let n = w * h;
        let mut rgb = vec![0f32; n * 3];
        let mut active = vec![false; n];
        let mut fixed = vec![false; n];
        for &(hx, hy) in &self.holes {
            let i = hy as usize * w + hx as usize;
            active[i] = true;
            rgb[i * 3..i * 3 + 3].copy_from_slice(&avg[..3]);
            let (x, y) = (hx as usize, hy as usize);
            let neigh = [
                (x > 0).then(|| i - 1),
                (x + 1 < w).then(|| i + 1),
                (y > 0).then(|| i - w),
                (y + 1 < h).then(|| i + w),
            ];
            for ni in neigh.into_iter().flatten() {
                if !self.hole[ni] && !fixed[ni] {
                    fixed[ni] = true;
                    active[ni] = true;
                    for c in 0..3 {
                        rgb[ni * 3 + c] = self.color[ni * 4 + c];
                    }
                }
            }
        }
        solve_harmonic_rgb(&mut rgb, &active, &fixed, w, h, 255.0);
        for &(hx, hy) in &self.holes {
            let i = hy as usize * w + hx as usize;
            let p = i * 4;
            self.color[p..p + 3].copy_from_slice(&rgb[i * 3..i * 3 + 3]);
            self.color[p + 3] = avg[3];
        }
        self.random_nnf();
    }

    /// Seed a finer level from the solved coarser one: bilinear-upsample the hole
    /// colour, and scale the coarse NNF ×2 as the starting correspondence.
    fn seed_from_coarser(
        &mut self,
        coarse_color: &[f32],
        coarse_nnf: &[(i32, i32)],
        cw: usize,
        ch: usize,
    ) {
        let sx = cw as f32 / self.w as f32;
        let sy = ch as f32 / self.h as f32;
        for &(hx, hy) in &self.holes {
            let fx = (hx as f32 + 0.5) * sx - 0.5;
            let fy = (hy as f32 + 0.5) * sy - 0.5;
            let col = bilinear(coarse_color, cw, ch, fx, fy);
            let p = (hy as usize * self.w + hx as usize) * 4;
            self.color[p..p + 4].copy_from_slice(&col);

            let pcx = ((hx as f32 + 0.5) * sx - 0.5)
                .round()
                .clamp(0.0, cw as f32 - 1.0) as usize;
            let pcy = ((hy as f32 + 0.5) * sy - 0.5)
                .round()
                .clamp(0.0, ch as f32 - 1.0) as usize;
            let (psx, psy) = coarse_nnf[pcy * cw + pcx];
            let cand = (
                (psx as f32 / sx).round() as i32,
                (psy as f32 / sy).round() as i32,
            );
            self.nnf[hy as usize * self.w + hx as usize] = if self.valid_at(cand.0, cand.1) {
                cand
            } else {
                self.random_valid_center(&mut Rng::new(
                    0x1234_5678 ^ ((hx as u32) << 16) ^ hy as u32,
                ))
            };
        }
    }

    fn random_nnf(&mut self) {
        let mut rng = Rng::new(0xDEAD_BEEF);
        let holes = self.holes.clone();
        for (hx, hy) in holes {
            let c = self.random_valid_center(&mut rng);
            self.nnf[hy as usize * self.w + hx as usize] = c;
        }
    }

    fn random_valid_center(&self, rng: &mut Rng) -> (i32, i32) {
        if self.valid_centers.is_empty() {
            return (self.hp as i32, self.hp as i32);
        }
        let idx = (rng.next() as usize) % self.valid_centers.len();
        self.valid_centers[idx]
    }

    /// One EM round: PatchMatch search, then parallel vote.
    fn solve(&mut self, iters: usize, rng: &mut Rng) {
        if self.valid_centers.is_empty() || self.holes.is_empty() {
            return;
        }
        for it in 0..iters {
            self.patchmatch_pass(it, rng);
            self.vote();
        }
    }

    /// Final reconstruction: every hole pixel copies the source pixel its own
    /// best patch is centred on. The averaged vote converges on structure but
    /// smooths away grain-like texture (pores, noise, foliage), which is what
    /// makes a fill look painted; copying real pixels keeps it. Neighbours
    /// map coherently after the EM passes, so structure stays continuous.
    fn take_best_match_pixels(&mut self) {
        for &(hx, hy) in &self.holes {
            let q = hy as usize * self.w + hx as usize;
            let (sx, sy) = self.nnf[q];
            if !self.valid_at(sx, sy) {
                continue;
            }
            let si = (sy as usize * self.w + sx as usize) * 4;
            let di = q * 4;
            for c in 0..4 {
                self.color[di + c] = self.color[si + c];
            }
        }
    }

    /// Patch SSD over RGB between a (possibly clamped) target patch centred at
    /// `(tx,ty)` and a *valid* source patch centred at `(sx,sy)`. Early-exits once
    /// the running cost passes `cutoff`.
    #[inline]
    fn patch_dist(&self, tx: i32, ty: i32, sx: i32, sy: i32, cutoff: f64) -> f64 {
        let hp = self.hp as i32;
        let w = self.w as i32;
        let h = self.h as i32;
        let mut sum = 0f64;
        for dy in -hp..=hp {
            let tyy = (ty + dy).clamp(0, h - 1);
            let syy = sy + dy;
            for dx in -hp..=hp {
                let txx = (tx + dx).clamp(0, w - 1);
                let sxx = sx + dx;
                let ti = (tyy as usize * self.w + txx as usize) * 4;
                let si = (syy as usize * self.w + sxx as usize) * 4;
                let dr = self.color[ti] - self.color[si];
                let dg = self.color[ti + 1] - self.color[si + 1];
                let db = self.color[ti + 2] - self.color[si + 2];
                sum += (dr * dr + dg * dg + db * db) as f64;
            }
            if sum >= cutoff {
                return sum;
            }
        }
        sum
    }

    fn patchmatch_pass(&mut self, iter: usize, rng: &mut Rng) {
        self.patchmatch_pass_biased(iter, rng, 1.0);
    }

    /// `coherence` < 1 lets a neighbour's shifted match win when it is within
    /// that factor of the best distance, growing larger coherent copies.
    fn patchmatch_pass_biased(&mut self, iter: usize, rng: &mut Rng, coherence: f64) {
        let n = self.holes.len();
        let forward = iter % 2 == 0;
        let dir: i32 = if forward { 1 } else { -1 };
        let max_radius = self.w.max(self.h) as i32;

        for k in 0..n {
            let (hx, hy) = if forward {
                self.holes[k]
            } else {
                self.holes[n - 1 - k]
            };
            let pidx = hy as usize * self.w + hx as usize;
            let (mut bsx, mut bsy) = self.nnf[pidx];
            let mut bd = self.patch_dist(hx, hy, bsx, bsy, f64::INFINITY);

            let nx = hx - dir;
            if nx >= 0 && nx < self.w as i32 && self.hole[hy as usize * self.w + nx as usize] {
                let (nsx, nsy) = self.nnf[hy as usize * self.w + nx as usize];
                let cand = (nsx + dir, nsy);
                if self.valid_at(cand.0, cand.1) {
                    let d = self.patch_dist(hx, hy, cand.0, cand.1, bd / coherence);
                    if d * coherence < bd {
                        bd = d;
                        bsx = cand.0;
                        bsy = cand.1;
                    }
                }
            }
            let ny = hy - dir;
            if ny >= 0 && ny < self.h as i32 && self.hole[ny as usize * self.w + hx as usize] {
                let (nsx, nsy) = self.nnf[ny as usize * self.w + hx as usize];
                let cand = (nsx, nsy + dir);
                if self.valid_at(cand.0, cand.1) {
                    let d = self.patch_dist(hx, hy, cand.0, cand.1, bd / coherence);
                    if d * coherence < bd {
                        bd = d;
                        bsx = cand.0;
                        bsy = cand.1;
                    }
                }
            }

            let mut radius = max_radius;
            while radius >= 1 {
                let rx = bsx + (rng.range(radius));
                let ry = bsy + (rng.range(radius));
                if self.valid_at(rx, ry) {
                    let d = self.patch_dist(hx, hy, rx, ry, bd);
                    if d < bd {
                        bd = d;
                        bsx = rx;
                        bsy = ry;
                    }
                }
                radius /= 2;
            }

            self.nnf[pidx] = (bsx, bsy);
            self.dist[pidx] = bd as f32;
        }
    }

    /// Reconstruct every hole pixel from the source pixels predicted by all
    /// overlapping hole-patch matches, each weighted by how well its patch
    /// matched (Wexler et al. 2007: w = exp(-d / 2σ²), σ² = 75th percentile of
    /// the match distances). A plain average blends good and poor matches and
    /// washes out fine texture (skin pores, grain); weighting keeps it. Reads
    /// only fixed source colours, so it is order-independent and parallel.
    fn vote(&mut self) {
        let w = self.w;
        let h = self.h;
        let hp = self.hp as i32;
        let color = &self.color;
        let hole = &self.hole;
        let nnf = &self.nnf;
        let dist = &self.dist;
        let sigma2 = {
            let mut d: Vec<f32> = self
                .holes
                .iter()
                .map(|&(x, y)| dist[y as usize * w + x as usize])
                .collect();
            let k = (d.len() * 3 / 4).min(d.len().saturating_sub(1));
            let (_, v, _) = d.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
            v.max(1.0)
        };

        let updates: Vec<((i32, i32), [f32; 4])> = self
            .holes
            .par_iter()
            .map(|&(qx, qy)| {
                let mut acc = [0f32; 4];
                let mut n = 0f32;
                for py in (qy - hp)..=(qy + hp) {
                    if py < 0 || py >= h as i32 {
                        continue;
                    }
                    for px in (qx - hp)..=(qx + hp) {
                        if px < 0 || px >= w as i32 {
                            continue;
                        }
                        let pi = py as usize * w + px as usize;
                        if !hole[pi] {
                            continue;
                        }
                        let (sx, sy) = nnf[pi];
                        let srcx = sx + (qx - px);
                        let srcy = sy + (qy - py);
                        let si = (srcy as usize * w + srcx as usize) * 4;
                        let wt = (-dist[pi] / (2.0 * sigma2)).exp().max(1e-6);
                        acc[0] += color[si] * wt;
                        acc[1] += color[si + 1] * wt;
                        acc[2] += color[si + 2] * wt;
                        acc[3] += color[si + 3] * wt;
                        n += wt;
                    }
                }
                let col = if n > 0.0 {
                    [acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n]
                } else {
                    [0.0, 0.0, 0.0, 0.0]
                };
                ((qx, qy), col)
            })
            .collect();

        for ((qx, qy), col) in updates {
            let p = (qy as usize * w + qx as usize) * 4;
            if col[3] == 0.0 && col[0] == 0.0 {
                continue;
            }
            self.color[p..p + 4].copy_from_slice(&col);
        }
    }
}

fn hole_bbox(hole: &[bool], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    let mut any = false;
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            if hole[row + x] {
                any = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if any {
        Some((x0, y0, x1, y1))
    } else {
        None
    }
}

/// A patch centred at `(x,y)` is a valid source patch iff it is fully in bounds
/// and none of its pixels are hole. Computed with a hole integral image so the
/// per-centre test is O(1).
fn compute_valid(hole: &[bool], w: usize, h: usize, hp: usize) -> Vec<bool> {
    let iw = w + 1;
    let mut integral = vec![0u32; iw * (h + 1)];
    for y in 0..h {
        let mut row_sum = 0u32;
        for x in 0..w {
            row_sum += hole[y * w + x] as u32;
            integral[(y + 1) * iw + (x + 1)] = integral[y * iw + (x + 1)] + row_sum;
        }
    }
    let rect_sum = |x0: usize, y0: usize, x1: usize, y1: usize| -> u32 {
        integral[y1 * iw + x1] + integral[y0 * iw + x0]
            - integral[y0 * iw + x1]
            - integral[y1 * iw + x0]
    };

    let mut valid = vec![false; w * h];
    if w <= 2 * hp || h <= 2 * hp {
        return valid;
    }
    for cy in hp..(h - hp) {
        for cx in hp..(w - hp) {
            let s = rect_sum(cx - hp, cy - hp, cx + hp + 1, cy + hp + 1);
            if s == 0 {
                valid[cy * w + cx] = true;
            }
        }
    }
    valid
}

/// Downscale a region's colour + hole mask by integer `factor`, returning a fresh
/// `Level`. A coarse pixel is "hole" if ≥50% of its source pixels were hole; its
/// colour is the box-average of the *source* (non-hole) children, falling back to
/// the plain box-average when the cell is all hole.
fn downscale_level(
    color: &[f32],
    hole: &[bool],
    w: usize,
    h: usize,
    factor: usize,
    patch: usize,
) -> Level {
    let nw = (w / factor).max(1);
    let nh = (h / factor).max(1);
    let mut ncolor = vec![0f32; nw * nh * 4];
    let mut nhole = vec![false; nw * nh];
    for ny in 0..nh {
        for nx in 0..nw {
            let mut acc_src = [0f64; 4];
            let mut n_src = 0u32;
            let mut acc_all = [0f64; 4];
            let mut n_all = 0u32;
            let mut n_hole = 0u32;
            for dy in 0..factor {
                let sy = ny * factor + dy;
                if sy >= h {
                    continue;
                }
                for dx in 0..factor {
                    let sx = nx * factor + dx;
                    if sx >= w {
                        continue;
                    }
                    let i = sy * w + sx;
                    let p = i * 4;
                    for c in 0..4 {
                        acc_all[c] += color[p + c] as f64;
                    }
                    n_all += 1;
                    if hole[i] {
                        n_hole += 1;
                    } else {
                        for c in 0..4 {
                            acc_src[c] += color[p + c] as f64;
                        }
                        n_src += 1;
                    }
                }
            }
            let di = (ny * nw + nx) * 4;
            if n_src > 0 {
                for c in 0..4 {
                    ncolor[di + c] = (acc_src[c] / n_src as f64) as f32;
                }
            } else if n_all > 0 {
                for c in 0..4 {
                    ncolor[di + c] = (acc_all[c] / n_all as f64) as f32;
                }
            }
            nhole[ny * nw + nx] = n_all > 0 && n_hole * 2 >= n_all;
        }
    }
    Level::new(ncolor, nhole, nw, nh, patch)
}

/// Bilinear sample of an RGBA f32 buffer with edge clamping.
fn bilinear(buf: &[f32], w: usize, h: usize, fx: f32, fy: f32) -> [f32; 4] {
    let x = fx.clamp(0.0, w as f32 - 1.0);
    let y = fy.clamp(0.0, h as f32 - 1.0);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let mut out = [0f32; 4];
    for c in 0..4 {
        let a = buf[(y0 * w + x0) * 4 + c];
        let b = buf[(y0 * w + x1) * 4 + c];
        let cc = buf[(y1 * w + x0) * 4 + c];
        let d = buf[(y1 * w + x1) * 4 + c];
        let top = a + (b - a) * tx;
        let bot = cc + (d - cc) * tx;
        out[c] = top + (bot - top) * ty;
    }
    out
}

/// Tiny deterministic xorshift32 PRNG (no external dep; PatchMatch only needs a
/// cheap, well-spread random source).
struct Rng(u32);
impl Rng {
    fn new(seed: u32) -> Self {
        Rng(seed | 1)
    }
    #[inline]
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// Uniform integer in [-radius, radius].
    #[inline]
    fn range(&mut self, radius: i32) -> i32 {
        if radius <= 0 {
            return 0;
        }
        let span = (radius as u32) * 2 + 1;
        (self.next() % span) as i32 - radius
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, rgba: [u8; 4]) -> Vec<u8> {
        (0..w * h).flat_map(|_| rgba).collect()
    }

    #[test]
    fn fills_solid_color_exactly() {
        let (w, h) = (48usize, 48usize);
        let mut img = solid(w, h, [40, 160, 90, 255]);
        let mut hole = vec![false; w * h];
        for y in 18..30 {
            for x in 18..30 {
                hole[y * w + x] = true;
                let i = (y * w + x) * 4;
                img[i] = 0;
                img[i + 1] = 0;
                img[i + 2] = 0;
            }
        }
        assert!(fill(&mut img, w, h, &hole));
        for y in 18..30 {
            for x in 18..30 {
                let i = (y * w + x) * 4;
                assert!((img[i] as i32 - 40).abs() <= 2, "r={}", img[i]);
                assert!((img[i + 1] as i32 - 160).abs() <= 2, "g={}", img[i + 1]);
                assert!((img[i + 2] as i32 - 90).abs() <= 2, "b={}", img[i + 2]);
            }
        }
    }

    #[test]
    fn leaves_source_pixels_untouched() {
        let (w, h) = (40usize, 40usize);
        let mut img: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % 251) as u8, (i % 113) as u8, (i % 97) as u8, 255])
            .collect();
        let original = img.clone();
        let mut hole = vec![false; w * h];
        for y in 16..24 {
            for x in 16..24 {
                hole[y * w + x] = true;
            }
        }
        fill(&mut img, w, h, &hole);
        for y in 0..h {
            for x in 0..w {
                if hole[y * w + x] {
                    continue;
                }
                let i = (y * w + x) * 4;
                assert_eq!(
                    &img[i..i + 4],
                    &original[i..i + 4],
                    "source changed at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn recovers_a_striped_texture() {
        let (w, h) = (64usize, 64usize);
        let truth: Vec<u8> = (0..w * h)
            .flat_map(|idx| {
                let x = idx % w;
                let v = if (x / 4) % 2 == 0 { 230 } else { 30 };
                [v, v, v, 255]
            })
            .collect();
        let mut img = truth.clone();
        let mut hole = vec![false; w * h];
        for y in 24..40 {
            for x in 24..40 {
                hole[y * w + x] = true;
                let i = (y * w + x) * 4;
                img[i] = 128;
                img[i + 1] = 128;
                img[i + 2] = 128;
            }
        }
        let err_before = hole_error(&img, &truth, &hole, w);
        fill(&mut img, w, h, &hole);
        let err_after = hole_error(&img, &truth, &hole, w);
        assert!(
            err_after < err_before * 0.5,
            "fill did not improve texture: before={err_before:.1} after={err_after:.1}"
        );
    }

    #[test]
    fn seamless_heal_matches_surrounding_colour() {
        let (w, h) = (64usize, 64usize);
        let bg = [70u8, 130, 180, 255];
        let mut img = solid(w, h, bg);
        let mut hole = vec![false; w * h];
        for y in 10..54 {
            for x in (y % 3 + 28)..(y % 3 + 31) {
                hole[y * w + x] = true;
                let i = (y * w + x) * 4;
                img[i] = 0;
                img[i + 1] = 0;
                img[i + 2] = 0;
            }
        }
        assert!(fill_seamless(&mut img, w, h, &hole));
        for y in 10..54 {
            for x in (y % 3 + 28)..(y % 3 + 31) {
                let i = (y * w + x) * 4;
                assert!((img[i] as i32 - bg[0] as i32).abs() <= 6, "r={}", img[i]);
                assert!(
                    (img[i + 1] as i32 - bg[1] as i32).abs() <= 6,
                    "g={}",
                    img[i + 1]
                );
                assert!(
                    (img[i + 2] as i32 - bg[2] as i32).abs() <= 6,
                    "b={}",
                    img[i + 2]
                );
            }
        }
        for y in 0..h {
            for x in 0..w {
                if hole[y * w + x] {
                    continue;
                }
                let i = (y * w + x) * 4;
                assert_eq!(&img[i..i + 4], &bg, "source changed at {x},{y}");
            }
        }
    }

    #[test]
    fn patch_clone_replaces_blemish_with_source() {
        let (w, h) = (64usize, 64usize);
        let bg = [70u8, 130, 180, 255];
        let mut img = solid(w, h, bg);
        // Mask rim sits on clean background; the black blemish is interior to it
        // (the realistic case — Poisson cloning bends the source to the rim colour).
        let mut mask = vec![0f32; w * h];
        for y in 26..38 {
            for x in 34..46 {
                mask[y * w + x] = 1.0;
            }
        }
        for y in 29..35 {
            for x in 37..43 {
                let i = (y * w + x) * 4;
                img[i] = 0;
                img[i + 1] = 0;
                img[i + 2] = 0;
            }
        }
        let before = img.clone();
        // Source is 20px to the left → clean background.
        assert!(seamless_clone_region(&mut img, w, h, &mask, -20, 0));
        for y in 26..38 {
            for x in 34..46 {
                let i = (y * w + x) * 4;
                assert!((img[i] as i32 - bg[0] as i32).abs() <= 8, "r={}", img[i]);
                assert!(
                    (img[i + 1] as i32 - bg[1] as i32).abs() <= 8,
                    "g={}",
                    img[i + 1]
                );
                assert!(
                    (img[i + 2] as i32 - bg[2] as i32).abs() <= 8,
                    "b={}",
                    img[i + 2]
                );
            }
        }
        // Pixels outside the mask must be untouched.
        for y in 0..h {
            for x in 0..w {
                if mask[y * w + x] >= 0.04 {
                    continue;
                }
                let i = (y * w + x) * 4;
                assert_eq!(
                    &img[i..i + 4],
                    &before[i..i + 4],
                    "outside changed at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn patch_clone_rejects_empty_mask() {
        let (w, h) = (8usize, 8usize);
        let mut img = solid(w, h, [200, 200, 200, 255]);
        let mask = vec![0f32; w * h];
        assert!(!seamless_clone_region(&mut img, w, h, &mask, 1, 1));
    }

    fn hole_error(img: &[u8], truth: &[u8], hole: &[bool], w: usize) -> f64 {
        let mut e = 0f64;
        for (i, &is_hole) in hole.iter().enumerate() {
            if !is_hole {
                continue;
            }
            let p = i * 4;
            for c in 0..3 {
                let d = img[p + c] as f64 - truth[p + c] as f64;
                e += d * d;
            }
        }
        (e / ((w * w) as f64)).sqrt()
    }
}

#[cfg(test)]
mod quality_tests {
    //! Quality floors for the content-aware fill, on synthetic scenes with a
    //! known ground truth: an object straddling two different backgrounds, and
    //! a blemish on pore-textured skin.
    use super::*;

    fn noise(i: usize, seed: u32) -> f32 {
        let mut x = (i as u32).wrapping_mul(2654435761) ^ seed;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x % 1000) as f32 / 1000.0 - 0.5
    }

    /// Sky-blue top half, grass-green bottom half, both with fine noise.
    fn two_backgrounds(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let n = noise(i, 7) * 18.0 + noise(i / 3, 9) * 10.0;
                let c = if y < h / 2 {
                    [110.0 + n, 160.0 + n, 220.0 + n * 0.5]
                } else {
                    [60.0 + n * 1.5, 140.0 + n * 1.5, 50.0 + n]
                };
                for k in 0..3 {
                    v[i * 4 + k] = c[k].clamp(0.0, 255.0) as u8;
                }
                v[i * 4 + 3] = 255;
            }
        }
        v
    }

    /// Skin tone with a gentle gradient, fine noise and 2-3 px pores.
    fn skin(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let (gx, gy) = (x / 5, y / 5);
                let cell = gy * 97 + gx;
                let (px, py) = (
                    gx * 5 + 1 + ((noise(cell, 21) + 0.5) * 3.0) as usize,
                    gy * 5 + 1 + ((noise(cell, 23) + 0.5) * 3.0) as usize,
                );
                let d2 = (x as f32 - px as f32).powi(2) + (y as f32 - py as f32).powi(2);
                let pore = if noise(cell, 25) > 0.1 {
                    -20.0 * (-d2 / 1.2).exp()
                } else {
                    0.0
                };
                let n = noise(i, 11) * 6.0 + pore + (x as f32 / w as f32) * 20.0;
                let c = [225.0 + n, 180.0 + n, 160.0 + n];
                for k in 0..3 {
                    v[i * 4 + k] = c[k].clamp(0.0, 255.0) as u8;
                }
                v[i * 4 + 3] = 255;
            }
        }
        v
    }

    /// Paint a red disc of radius `r` and return the hole (disc + 3 px).
    fn paint_disc(img: &mut [u8], w: usize, h: usize, c: (f32, f32), r: f32) -> Vec<bool> {
        let mut hole = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 + 0.5 - c.0).powi(2) + (y as f32 + 0.5 - c.1).powi(2)).sqrt();
                if d < r + 3.0 {
                    hole[y * w + x] = true;
                }
                if d < r {
                    img[(y * w + x) * 4..][..3].copy_from_slice(&[200, 40, 40]);
                }
            }
        }
        hole
    }

    fn rms(img: &[u8], truth: &[u8], hole: &[bool]) -> f64 {
        let (mut e, mut n) = (0f64, 0f64);
        for (i, _) in hole.iter().enumerate().filter(|(_, &h)| h) {
            for c in 0..3 {
                let d = img[i * 4 + c] as f64 - truth[i * 4 + c] as f64;
                e += d * d;
                n += 1.0;
            }
        }
        (e / n).sqrt()
    }

    /// Mean |Laplacian| of luma inside the hole: fine-detail energy.
    fn detail(img: &[u8], hole: &[bool], w: usize) -> f64 {
        let l =
            |i: usize| (img[i * 4] as f64 + img[i * 4 + 1] as f64 + img[i * 4 + 2] as f64) / 3.0;
        let (mut acc, mut n) = (0f64, 0f64);
        for (i, &hh) in hole.iter().enumerate() {
            let (x, y) = (i % w, i / w);
            if !hh || x == 0 || y == 0 || x + 1 >= w || i + w >= hole.len() {
                continue;
            }
            acc += (4.0 * l(i) - l(i - 1) - l(i + 1) - l(i - w) - l(i + w)).abs();
            n += 1.0;
        }
        acc / n
    }

    #[test]
    fn object_removal_follows_both_backgrounds_and_keeps_detail() {
        let (w, h) = (240usize, 200usize);
        let truth = two_backgrounds(w, h);
        let mut img = truth.clone();
        let hole = paint_disc(&mut img, w, h, (120.0, 100.0), 34.0);
        assert!(fill_seamless(&mut img, w, h, &hole));
        // A flat-average start used to smear the two halves together (rms ≈ 17).
        let e = rms(&img, &truth, &hole);
        assert!(
            e < 11.0,
            "fill strays from the true backgrounds: rms {e:.1}"
        );
        // Averaged votes used to keep only ~63 % of the fine detail.
        let (got, want) = (detail(&img, &hole, w), detail(&truth, &hole, w));
        assert!(
            got > 0.85 * want,
            "fill too smooth: detail {got:.1} vs {want:.1}"
        );
    }

    #[test]
    fn blemish_fill_keeps_skin_texture() {
        let (w, h) = (240usize, 200usize);
        let truth = skin(w, h);
        let mut img = truth.clone();
        let hole = paint_disc(&mut img, w, h, (120.0, 100.0), 12.0);
        assert!(fill_seamless(&mut img, w, h, &hole));
        // The old fill kept about half of the pores (a plastic-looking patch).
        let (got, want) = (detail(&img, &hole, w), detail(&truth, &hole, w));
        assert!(
            got > 0.7 * want,
            "skin texture lost: detail {got:.1} vs {want:.1}"
        );
    }

    #[test]
    fn refine_restores_real_texture_under_a_blurry_guide() {
        let (w, h) = (240usize, 200usize);
        let truth = two_backgrounds(w, h);
        let mut img = truth.clone();
        let hole = paint_disc(&mut img, w, h, (120.0, 100.0), 34.0);
        // A 7x7 box blur of the truth in the hole: what a low-resolution AI
        // fill looks like — right structure, no grain.
        let mut guide = truth.clone();
        for y in 0..h {
            for x in 0..w {
                if !hole[y * w + x] {
                    continue;
                }
                for c in 0..3 {
                    let (mut acc, mut n) = (0u32, 0u32);
                    for yy in y.saturating_sub(3)..(y + 4).min(h) {
                        for xx in x.saturating_sub(3)..(x + 4).min(w) {
                            acc += truth[(yy * w + xx) * 4 + c] as u32;
                            n += 1;
                        }
                    }
                    guide[(y * w + x) * 4 + c] = (acc / n) as u8;
                }
            }
        }
        let before = rms(&guide, &truth, &hole);
        assert!(refine_fill(&mut guide, w, h, &hole));
        let (got, want) = (detail(&guide, &hole, w), detail(&truth, &hole, w));
        assert!(
            got > 0.8 * want,
            "grain not restored: detail {got:.1} vs {want:.1}"
        );
        let after = rms(&guide, &truth, &hole);
        assert!(
            after < before,
            "refine drifted from the guide: {before:.1} -> {after:.1}"
        );
    }

    /// Needs the local LaMa model; run with `--ignored` to compare the AI
    /// path with PatchMatch on the object scene.
    #[test]
    #[ignore]
    fn ai_spot_fill_on_object_scene() {
        if !crate::core::lama::is_available() {
            return;
        }
        let (w, h) = (480usize, 400usize);
        let truth = two_backgrounds(w, h);
        let mut img = truth.clone();
        let hole = paint_disc(&mut img, w, h, (240.0, 200.0), 70.0);
        let cover: Vec<f32> = hole.iter().map(|&b| if b { 1.0 } else { 0.0 }).collect();
        for use_ai in [false, true] {
            let mut a = img.clone();
            let t = std::time::Instant::now();
            assert!(fill_spot_with(&mut a, w, h, &cover, 1.0, use_ai));
            println!(
                "ai={use_ai} rms={:.1} detail={:.1} (truth {:.1}) {:?}",
                rms(&a, &truth, &hole),
                detail(&a, &hole, w),
                detail(&truth, &hole, w),
                t.elapsed()
            );
        }
    }

    #[test]
    fn spot_fill_heals_the_whole_soft_footprint() {
        let (w, h) = (96usize, 96usize);
        let bg = [180u8, 140, 120, 255];
        let mut img: Vec<u8> = (0..w * h).flat_map(|_| bg).collect();
        let (c, r) = (48.0f32, 16.0f32);
        let mut cover = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
                // A fully soft tip: coverage fades to 0 at the rim.
                cover[y * w + x] = (1.0 - d / r).max(0.0);
                if d < r * 0.8 {
                    img[(y * w + x) * 4..][..3].copy_from_slice(&[90, 30, 30]);
                }
            }
        }
        let before = img.clone();
        assert!(fill_spot(&mut img, w, h, &cover, 1.0));
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                if cover[y * w + x] >= SPOT_HEAL_COVER {
                    // The blemish reaches 80 % of the radius, where a soft tip's
                    // coverage is only 0.2: it must still be healed completely.
                    for k in 0..3 {
                        assert!(
                            (img[i + k] as i32 - bg[k] as i32).abs() <= 6,
                            "({x},{y}) still shows the blemish: {:?}",
                            &img[i..i + 3]
                        );
                    }
                } else {
                    let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
                    if d > r * 0.9 + 4.0 {
                        assert_eq!(&img[i..i + 4], &before[i..i + 4], "({x},{y}) changed");
                    }
                }
            }
        }
    }
}
