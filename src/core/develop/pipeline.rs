//! Display-domain apply pipeline and its tone-LUT engine.
//!
//! `apply_to_tilemap_direct` runs the full per-tile chain (tone -> colour ->
//! effects -> detail -> locals) on a display-referred tilemap. The scene-referred
//! path (`develop_scene.rs`) performs Light physically and then calls this with
//! neutral Light so only Colour/Effects/Detail/Locals apply; the tone-LUT builders
//! here drive the display-domain fallback used for layers too large for the GPU.

use super::*;
use crate::core::color::luminance_f32;
use crate::core::tile::{dither16_to_u8, quantize_dither, TileMap, TILE_PIXELS, TILE_SIZE};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

/// Flat-buffer entry point exercised by the unit tests. Production applies Camera
/// Raw per tile through `apply_to_tilemap_direct`.
#[cfg(test)]
pub fn apply_to_pixels(settings: &DevelopSettings, pixels: &mut [u8], width: u32, height: u32) {
    if settings.is_neutral() || width == 0 || height == 0 {
        return;
    }

    let plan = DevelopPlan::new(settings, width, height);

    pixels
        .par_chunks_exact_mut(4)
        .enumerate()
        .for_each(|(idx, px)| {
            if px[3] == 0 {
                return;
            }

            let x = (idx as u32) % width;
            let y = (idx as u32) / width;
            let out = plan.apply_pixel(px[0], px[1], px[2], px[3], x, y, None);
            px[0] = out[0];
            px[1] = out[1];
            px[2] = out[2];
        });

    if has_detail(settings) {
        apply_detail_to_pixels(settings, pixels, width, height);
    }
}

#[derive(Clone)]
pub struct DevelopSelection {
    pub selection: Arc<crate::core::selection::Selection>,
    pub layer_offset: (i32, i32),
}

pub fn apply_to_tilemap_direct(
    source_tiles: &TileMap,
    settings: &DevelopSettings,
    selection: Option<DevelopSelection>,
) -> TileMap {
    if settings.is_neutral() {
        return source_tiles.clone();
    }

    let plan = DevelopPlan::new(settings, source_tiles.width, source_tiles.height);

    // Regional base luminance is needed by local-adaptation tone AND by the
    // spatial effects (Clarity/Defog) — see `build_base_luma`.
    let needs_base =
        plan.tone.as_ref().is_some_and(|t| t.is_local) || settings.has_spatial_effects();

    // 16-bit fast path: when the bake reduces to the global per-pixel stage (no
    // per-band Colour, no regional base, no spatial Detail), apply at full
    // 16-bit precision with an interpolated tone LUT — so a 16-bit document keeps
    // continuous tone (no banding). The richer spatial stages still bake at 8-bit.
    let pure_global = !plan.use_color && !needs_base && !has_detail(settings);
    if pure_global && source_tiles.has_hdr() {
        let w = source_tiles.width;
        let h = source_tiles.height;
        let mut px16 = source_tiles.flatten16();
        px16.par_chunks_exact_mut(4)
            .enumerate()
            .for_each(|(idx, px)| {
                if px[3] == 0 {
                    return;
                }
                let x = (idx as u32) % w;
                let y = (idx as u32) / w;
                let out = plan.apply_pixel16(px[0], px[1], px[2], px[3], x, y, None);
                if let Some(sel) = &selection {
                    let cx = sel.layer_offset.0 + x as i32;
                    let cy = sel.layer_offset.1 + y as i32;
                    let sel_a = if cx >= 0 && cy >= 0 {
                        sel.selection.sample(cx as u32, cy as u32)
                    } else {
                        0.0
                    };
                    if sel_a <= 0.001 {
                        return;
                    }
                    if sel_a >= 0.999 {
                        px[0] = out[0];
                        px[1] = out[1];
                        px[2] = out[2];
                    } else {
                        let inv = 1.0 - sel_a;
                        px[0] = (out[0] as f32 * sel_a + px[0] as f32 * inv).round() as u16;
                        px[1] = (out[1] as f32 * sel_a + px[1] as f32 * inv).round() as u16;
                        px[2] = (out[2] as f32 * sel_a + px[2] as f32 * inv).round() as u16;
                    }
                } else {
                    px[0] = out[0];
                    px[1] = out[1];
                    px[2] = out[2];
                }
            });
        let mut out = TileMap::from_rgba16(&px16, w, h);
        out.bump_all_revisions();
        return out;
    }

    // 16-bit per-tile path for the spatial stages (per-band Colour and/or
    // local-adaptation Shadows/Highlights) — same structure as the 8-bit loop, but
    // reads the source at 16-bit, interpolates the tone LUTs and writes the 16-bit
    // master. Detail runs as a second 16-bit-aware pass at the end.
    if source_tiles.has_hdr() {
        let tiles: HashMap<_, _> = source_tiles
            .tiles
            .par_iter()
            .map(|(pos, arc_tile)| {
                let mut tile = (**arc_tile).clone();
                let base_x = pos.x.max(0) as u32 * TILE_SIZE;
                let base_y = pos.y.max(0) as u32 * TILE_SIZE;
                let valid_w = source_tiles.width.saturating_sub(base_x).min(TILE_SIZE);
                let valid_h = source_tiles.height.saturating_sub(base_y).min(TILE_SIZE);

                // Built whenever the tone is local (H/S/W/B) OR spatial effects
                // need it — including under the Colour Mixer, so the colour path's
                // per-pixel `toned` can lift Shadows/Blacks regionally.
                let base_luma = if valid_w > 0 && valid_h > 0 && needs_base {
                    Some(build_base_luma(
                        source_tiles,
                        plan.tone.as_ref(),
                        base_x,
                        base_y,
                        valid_w,
                        valid_h,
                    ))
                } else {
                    None
                };
                let color_bufs = if plan.use_color
                    && plan.settings.color_smoothing > 0.001
                    && valid_w > 0
                    && valid_h > 0
                {
                    Some(build_color_lowpass(
                        source_tiles,
                        &plan.tone,
                        plan.settings,
                        base_x,
                        base_y,
                        valid_w,
                        valid_h,
                        true, // 16-bit: interpolated tone LUT
                        base_luma.as_deref(),
                    ))
                } else {
                    None
                };

                let p16 = tile
                    .pixels16
                    .get_or_insert_with(|| tile.pixels.iter().map(|&v| v as u16 * 257).collect());
                for ty in 0..valid_h {
                    for tx in 0..valid_w {
                        let i = ((ty * TILE_SIZE + tx) * 4) as usize;
                        let a = p16[i + 3];
                        if a == 0 {
                            continue;
                        }
                        let lx = base_x + tx;
                        let ly = base_y + ty;
                        let out: [u16; 3] = if let Some((toned, region, adjusted)) = &color_bufs {
                            let ci = (ty * valid_w + tx) as usize;
                            plan.finish_colored_pixel16(toned[ci], region[ci], adjusted[ci], lx, ly)
                        } else {
                            let bi = (ty * valid_w + tx) as usize;
                            let bl = base_luma.as_ref().map(|b| b[bi]);
                            let o =
                                plan.apply_pixel16(p16[i], p16[i + 1], p16[i + 2], a, lx, ly, bl);
                            [o[0], o[1], o[2]]
                        };

                        if let Some(sel) = &selection {
                            let cx = sel.layer_offset.0 + lx as i32;
                            let cy = sel.layer_offset.1 + ly as i32;
                            let sel_a = if cx >= 0 && cy >= 0 {
                                sel.selection.sample(cx as u32, cy as u32)
                            } else {
                                0.0
                            };
                            if sel_a <= 0.001 {
                                continue;
                            }
                            if sel_a >= 0.999 {
                                p16[i] = out[0];
                                p16[i + 1] = out[1];
                                p16[i + 2] = out[2];
                            } else {
                                let inv = 1.0 - sel_a;
                                p16[i] =
                                    (out[0] as f32 * sel_a + p16[i] as f32 * inv).round() as u16;
                                p16[i + 1] = (out[1] as f32 * sel_a + p16[i + 1] as f32 * inv)
                                    .round() as u16;
                                p16[i + 2] = (out[2] as f32 * sel_a + p16[i + 2] as f32 * inv)
                                    .round() as u16;
                            }
                        } else {
                            p16[i] = out[0];
                            p16[i + 1] = out[1];
                            p16[i + 2] = out[2];
                        }
                    }
                }
                // Refresh the 8-bit display mirror from the updated 16-bit master,
                // ordered-dithered so the committed result matches the live preview's
                // dither and smooth gradients don't posterize (tile-local coords are
                // seam-free since TILE_SIZE is a multiple of the 8×8 Bayer period).
                if let Some(p16) = &tile.pixels16 {
                    for p in 0..TILE_PIXELS {
                        let x = (p % TILE_SIZE as usize) as u32;
                        let y = (p / TILE_SIZE as usize) as u32;
                        let i = p * 4;
                        tile.pixels[i] = dither16_to_u8(p16[i], x, y, 0);
                        tile.pixels[i + 1] = dither16_to_u8(p16[i + 1], x, y, 1);
                        tile.pixels[i + 2] = dither16_to_u8(p16[i + 2], x, y, 2);
                        tile.pixels[i + 3] = (p16[i + 3] >> 8) as u8;
                    }
                }
                (*pos, Arc::new(tile))
            })
            .collect();
        let mut out = TileMap {
            tiles,
            width: source_tiles.width,
            height: source_tiles.height,
        };
        if has_detail(settings) {
            out = apply_detail_to_tilemap(&out, settings);
        }
        out.bump_all_revisions();
        return out;
    }

    let tiles: HashMap<_, _> = source_tiles
        .tiles
        .par_iter()
        .map(|(pos, arc_tile)| {
            let mut tile = (**arc_tile).clone();
            let base_x = pos.x.max(0) as u32 * TILE_SIZE;
            let base_y = pos.y.max(0) as u32 * TILE_SIZE;
            let valid_w = source_tiles.width.saturating_sub(base_x).min(TILE_SIZE);
            let valid_h = source_tiles.height.saturating_sub(base_y).min(TILE_SIZE);

            // Regional base luminance per tile: local-adaptation tone and the
            // spatial effects (Clarity/Defog) read it. Built whenever needed —
            // including under the Colour Mixer, so the colour path's per-pixel
            // `toned` can lift Shadows/Blacks regionally (no jump vs tone-only).
            let base_luma = if valid_w > 0 && valid_h > 0 && needs_base {
                Some(build_base_luma(
                    source_tiles,
                    plan.tone.as_ref(),
                    base_x,
                    base_y,
                    valid_w,
                    valid_h,
                ))
            } else {
                None
            };

            // Detail-preserving colour stage: boost the low-frequency content and
            // re-add (attenuated) detail so a steep per-band saturation/luminance
            // push does not magnify the source JPEG's chroma blocks into patches.
            // Built once per tile, reading a halo from the source for seam-free blur.
            let color_bufs = if plan.use_color
                && plan.settings.color_smoothing > 0.001
                && valid_w > 0
                && valid_h > 0
            {
                Some(build_color_lowpass(
                    source_tiles,
                    &plan.tone,
                    plan.settings,
                    base_x,
                    base_y,
                    valid_w,
                    valid_h,
                    false, // 8-bit path: hard-indexed tone LUT (unchanged)
                    base_luma.as_deref(),
                ))
            } else {
                None
            };

            for ty in 0..valid_h {
                for tx in 0..valid_w {
                    let i = ((ty * TILE_SIZE + tx) * 4) as usize;
                    let a = tile.pixels[i + 3];
                    if a == 0 {
                        continue;
                    }

                    let lx = base_x + tx;
                    let ly = base_y + ty;
                    let out = if let Some((toned_buf, region_buf, adjusted_buf)) = &color_bufs {
                        let ci = (ty * valid_w + tx) as usize;
                        let rgb = plan.finish_colored_pixel(
                            toned_buf[ci],
                            region_buf[ci],
                            adjusted_buf[ci],
                            lx,
                            ly,
                        );
                        [rgb[0], rgb[1], rgb[2], a]
                    } else {
                        let bi = (ty * valid_w + tx) as usize;
                        let bl = base_luma.as_ref().map(|b| b[bi]);
                        plan.apply_pixel(
                            tile.pixels[i],
                            tile.pixels[i + 1],
                            tile.pixels[i + 2],
                            a,
                            lx,
                            ly,
                            bl,
                        )
                    };

                    if let Some(sel) = &selection {
                        let cx = sel.layer_offset.0 + lx as i32;
                        let cy = sel.layer_offset.1 + ly as i32;
                        let sel_a = if cx >= 0 && cy >= 0 {
                            sel.selection.sample(cx as u32, cy as u32)
                        } else {
                            0.0
                        };
                        if sel_a <= 0.001 {
                            continue;
                        }
                        if sel_a >= 0.999 {
                            tile.pixels[i] = out[0];
                            tile.pixels[i + 1] = out[1];
                            tile.pixels[i + 2] = out[2];
                        } else {
                            let inv = 1.0 - sel_a;
                            tile.pixels[i] =
                                (out[0] as f32 * sel_a + tile.pixels[i] as f32 * inv).round() as u8;
                            tile.pixels[i + 1] = (out[1] as f32 * sel_a
                                + tile.pixels[i + 1] as f32 * inv)
                                .round() as u8;
                            tile.pixels[i + 2] = (out[2] as f32 * sel_a
                                + tile.pixels[i + 2] as f32 * inv)
                                .round() as u8;
                        }
                    } else {
                        tile.pixels[i] = out[0];
                        tile.pixels[i + 1] = out[1];
                        tile.pixels[i + 2] = out[2];
                    }
                }
            }
            (*pos, Arc::new(tile))
        })
        .collect();

    let mut out = TileMap {
        tiles,
        width: source_tiles.width,
        height: source_tiles.height,
    };
    if has_detail(settings) {
        out = apply_detail_to_tilemap(&out, settings);
    }
    out.bump_all_revisions();
    out
}

/// Precomputed tone stage shared by the CPU bake and the GPU preview.
///
/// The whole Light + Curve panel (White Balance, Exposure, Contrast,
/// Highlights/Shadows/Whites/Blacks and the point curve) collapses into:
///   • three per-channel linear gains (`gains`) — White Balance, luma-normalised
///   • one linear exposure multiplier (`ev`) — ×2^EV
///   • one 256-entry tone curve (`lut`) over the luminance axis.
///
/// This is the SINGLE SOURCE OF TRUTH: the GPU shader receives the exact same
/// `gains`/`ev`/`lut` (see `develop_to_gpu`) and runs the identical math, so
/// the live preview and the committed pixels match. The tonal curve is applied to
/// luminance and the original colour is re-applied around the new luma
/// (`apply_luma_target`), so lifting shadows or recovering highlights keeps the
/// colour instead of washing toward grey.
pub(crate) struct ToneData {
    pub gains: [f32; 3],
    pub ev: f32,
    pub lut: [f32; 256],
    /// Global tone curve WITHOUT the Highlights/Shadows terms — used only by the
    /// local-adaptation path (`apply_local`), where those two are applied from the
    /// regional base luminance instead. Empty/unused when `is_local` is false.
    pub global_lut: [f32; 256],
    /// Highlights/Shadows contribution as a signed luma OFFSET vs the input luma,
    /// evaluated at the *regional* (blurred) luminance in `apply_local`. Applying
    /// it from the region instead of the pixel lifts a whole dark area uniformly,
    /// preserving its local contrast (no muddy flattening).
    pub local_lut: [f32; 256],
    /// True when Highlights or Shadows are in play → use the local-adaptation
    /// path. (Those also force the CPU bake; see `DevelopSettings::has_local_tone`.)
    pub is_local: bool,
    /// Per-channel R/G/B point-curve LUTs, applied after the luma tone stage;
    /// None when all three channel curves are identity. The GPU preview reads
    /// the same tables via the `dev_rgb_curve` storage buffer.
    pub rgb: Option<Box<[[f32; 256]; 3]>>,
}

impl ToneData {
    /// White Balance + Exposure as per-channel linear gains (with highlight
    /// roll-off), in [0,1]. The shared first half of both tone paths.
    /// The rolloff is ALWAYS applied to luma and the RGB vector is scaled
    /// proportionally so hue and saturation are preserved regardless of EV
    /// direction — the old per-channel fallback shifted hue on bright colours.
    #[inline]
    pub(crate) fn apply_wb_ev(&self, r: &mut f32, g: &mut f32, b: &mut f32) {
        let mut lr = srgb_to_linear(*r) * self.gains[0] * self.ev;
        let mut lg = srgb_to_linear(*g) * self.gains[1] * self.ev;
        let mut lb = srgb_to_linear(*b) * self.gains[2] * self.ev;
        let l0 = luma_lin(lr, lg, lb).max(0.0);
        let target = highlight_rolloff(l0);
        if l0 > 1e-6 {
            let scale = target / l0;
            lr *= scale;
            lg *= scale;
            lb *= scale;
        }
        [lr, lg, lb] = fit_linear_rgb_to_luma([lr, lg, lb], target);
        *r = linear_to_srgb(lr);
        *g = linear_to_srgb(lg);
        *b = linear_to_srgb(lb);
        clamp_unit(r, g, b);
    }

    /// Global tone stage for one pixel (sRGB channels in [0,1]). Mirrors the WGSL
    /// `develop_apply` exactly: WB+Exposure, then the unified tone curve on
    /// luminance, then re-apply chroma around the new luma. Used when there is no
    /// local adaptation (and on the GPU).
    /// Apply the per-channel R/G/B point curves (always interpolated — a hard
    /// index would band the smooth channel maps). No-op when identity.
    #[inline]
    fn apply_rgb_curves(&self, r: &mut f32, g: &mut f32, b: &mut f32) {
        if let Some(luts) = &self.rgb {
            *r = lut_lerp(&luts[0], r.clamp(0.0, 1.0));
            *g = lut_lerp(&luts[1], g.clamp(0.0, 1.0));
            *b = lut_lerp(&luts[2], b.clamp(0.0, 1.0));
        }
    }

    #[inline]
    pub(crate) fn apply(&self, r: &mut f32, g: &mut f32, b: &mut f32) {
        self.apply_wb_ev(r, g, b);
        let l = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
        let idx = (l * 255.0 + 0.5) as usize;
        let target = self.lut[idx.min(255)];
        apply_luma_target(r, g, b, target);
        self.apply_rgb_curves(r, g, b);
    }

    /// Like [`Self::apply`] but linearly interpolates the 256-entry tone LUT, so a
    /// 16-bit input keeps continuous tone (no 256-step banding on the curve).
    pub(crate) fn apply_interp(&self, r: &mut f32, g: &mut f32, b: &mut f32) {
        self.apply_wb_ev(r, g, b);
        let l = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
        let target = lut_lerp(&self.lut, l);
        apply_luma_target(r, g, b, target);
        self.apply_rgb_curves(r, g, b);
    }

    /// Local-adaptation tone stage (Shadows/Highlights). `base_luma` is the pixel's
    /// edge-aware regional luminance. The global curve is read at the pixel's own
    /// luma (keeps detail/contrast) while the Highlights/Shadows offset is read at
    /// the REGIONAL luma — so a dark area is lifted as a whole instead of each
    /// pixel by its own value. On top of that, `local_detail_boost` re-amplifies
    /// the pixel's fine deviation from the region in proportion to how far the
    /// base moved: a purely additive lift keeps ABSOLUTE detail amplitude, which
    /// reads as flat/muddy at the new brightness (Weber) — scaling it by the
    /// base's ratio keeps the PERCEIVED texture, like a real raw developer.
    #[inline]
    pub(crate) fn apply_local(&self, r: &mut f32, g: &mut f32, b: &mut f32, base_luma: f32) {
        self.apply_wb_ev(r, g, b);
        let l = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
        let base = base_luma.clamp(0.0, 1.0);
        let gi = (l * 255.0 + 0.5) as usize;
        let bi = (base * 255.0 + 0.5) as usize;
        let offset = self.local_lut[bi.min(255)];
        let target = (self.global_lut[gi.min(255)] + offset + local_detail_boost(l, base, offset))
            .clamp(0.0, 1.0);
        apply_luma_target(r, g, b, target);
        self.apply_rgb_curves(r, g, b);
    }

    /// Interpolated-LUT variant of [`Self::apply_local`] for 16-bit input (no
    /// 256-step banding on the Shadows/Highlights curves).
    #[inline]
    pub(crate) fn apply_local_interp(&self, r: &mut f32, g: &mut f32, b: &mut f32, base_luma: f32) {
        self.apply_wb_ev(r, g, b);
        let l = luminance_f32(*r, *g, *b).clamp(0.0, 1.0);
        let base = base_luma.clamp(0.0, 1.0);
        let offset = lut_lerp(&self.local_lut, base);
        let target = (lut_lerp(&self.global_lut, l) + offset + local_detail_boost(l, base, offset))
            .clamp(0.0, 1.0);
        apply_luma_target(r, g, b, target);
        self.apply_rgb_curves(r, g, b);
    }

    /// Map the effects' regional base luminance through the same tone curve a
    /// pixel of that luminance receives, so Clarity/Defog compare post-tone
    /// pixels against a post-tone base (the WB+Exposure half is already baked
    /// into the base by `build_base_luma`). Always interpolated — the base is a
    /// smooth field and hard indexing would posterize it.
    fn map_luma_ref(&self, l: f32) -> f32 {
        let l = l.clamp(0.0, 1.0);
        let mapped = if self.is_local {
            (lut_lerp(&self.global_lut, l) + lut_lerp(&self.local_lut, l)).clamp(0.0, 1.0)
        } else {
            lut_lerp(&self.lut, l).clamp(0.0, 1.0)
        };
        // The channel curves shift luminance too; track them on the base field
        // by mapping a neutral grey of that luminance through them.
        if let Some(luts) = &self.rgb {
            luminance_f32(
                lut_lerp(&luts[0], mapped),
                lut_lerp(&luts[1], mapped),
                lut_lerp(&luts[2], mapped),
            )
            .clamp(0.0, 1.0)
        } else {
            mapped
        }
    }
}

pub(crate) struct DevelopPlan<'a> {
    settings: &'a DevelopSettings,
    inv_w: f32,
    inv_h: f32,
    tone: Option<ToneData>,
    use_color: bool,
    use_effects: bool,
    /// Interpolated mixer curves (`Some` when any band slider is engaged) and
    /// whether the full-res anti-bleed re-gate applies (band edit without a
    /// global Vibrance/Saturation — see `mixer_edit_mask`).
    mixer_curves: Option<MixerCurves>,
    mixer_gated: bool,
    /// Precomputed stages for the non-neutral local-adjustment masks, applied
    /// after every global stage (see `apply_locals`).
    locals: Vec<LocalPlan>,
}

/// Precomputed per-mask stage for one local adjustment: the mask's sliders as
/// synthetic global settings so the local stage reuses the exact tone/colour
/// builders of the global path.
struct LocalPlan {
    shape: LocalMaskShape,
    settings: DevelopSettings,
    tone: Option<ToneData>,
    use_color: bool,
    mixer_curves: Option<MixerCurves>,
}

/// Which mixer bands are edited, or None when the Colour edit is global
/// (Vibrance/Saturation touch every hue — the bleed gate must stay open) or
/// when no band is edited at all. Shared by the CPU bake plan and the GPU
/// effects-buffer upload so both sides gate identically.
pub(crate) fn mixer_edit_mask(settings: &DevelopSettings) -> Option<[bool; MIXER_BANDS]> {
    let mut edited = [false; MIXER_BANDS];
    let mut any = false;
    for i in 0..MIXER_BANDS {
        edited[i] = settings.mixer_hue[i].abs() > 0.001
            || settings.mixer_saturation[i].abs() > 0.001
            || settings.mixer_luminance[i].abs() > 0.001;
        any |= edited[i];
    }
    let global_color = settings.vibrance.abs() > 0.001 || settings.saturation.abs() > 0.001;
    (any && !global_color).then_some(edited)
}

/// Full-resolution spatial-bleed guard for mixer-band edits. The colour stage
/// runs on a low-res proxy, so at a colour boundary the adjusted region leaks
/// across the edge; re-gating the per-pixel delta by the PIXEL's own edited-hue
/// membership (the gate LUT is built from the same Lagrange basis the mixer
/// curves interpolate on) times the saturation weight keeps a Red push inside
/// the red object — a neighbouring pixel of another hue, or a neutral one,
/// takes none of the edit. Mirrored in the WGSL `dev_mixer_edit_affinity`
/// (which samples the SAME uploaded gate LUT).
pub(crate) fn band_affinity(curves: &MixerCurves, r: f32, g: f32, b: f32) -> f32 {
    if curves.algorithm == ColorMixerAlgorithm::V2 {
        let lab = crate::core::perceptual_color::linear_srgb_to_oklab([
            srgb_to_linear(r),
            srgb_to_linear(g),
            srgb_to_linear(b),
        ]);
        let p = crate::core::perceptual_color::PerceptualColor::from_oklab(lab);
        let gate = curve_sample(&curves.gate, p.hue);
        return (gate * smootherstep(0.008, 0.055, p.chroma)).clamp(0.0, 1.0);
    }
    let gate = curve_sample(&curves.gate, crate::core::ucs::ucs_hue_rad(r, g, b));
    let w = mixer_weight(r, g, b, MIXER_SAT_SHIFT);
    (gate * w).clamp(0.0, 1.0)
}

pub(crate) fn mixer_edit_affinity(curves: &MixerCurves, region: [f32; 3]) -> f32 {
    // Spatially-SMOOTH membership from the guided low-pass `region`, a
    // "prefilter chromaticity" step: the band selection reads the
    // neighbourhood hue, not the sharp per-pixel hue, so it can't flip pixel to
    // pixel across a JPEG chroma block or a soft edge — the edit tapers over the
    // region's resolution instead of a hard 1-px line. A UNIFORM object's
    // low-pass equals its pixels, so its interior takes exactly its previous
    // edit (only boundaries soften); membership already excludes neutrals.
    smootherstep(
        REGATE_LO,
        REGATE_HI,
        band_affinity(curves, region[0], region[1], region[2]),
    )
}

fn mixer_desat_affinity(curves: &MixerCurves, region: [f32; 3]) -> f32 {
    if curves.algorithm == ColorMixerAlgorithm::V2 {
        let lab = crate::core::perceptual_color::linear_srgb_to_oklab([
            srgb_to_linear(region[0]),
            srgb_to_linear(region[1]),
            srgb_to_linear(region[2]),
        ]);
        let p = crate::core::perceptual_color::PerceptualColor::from_oklab(lab);
        return (curve_sample(&curves.gate, p.hue) * smootherstep(0.002, 0.025, p.chroma))
            .clamp(0.0, 1.0);
    }
    let gate = curve_sample(
        &curves.gate,
        crate::core::ucs::ucs_hue_rad(region[0], region[1], region[2]),
    );
    smootherstep(
        REGATE_LO,
        REGATE_HI,
        (gate * mixer_desat_weight(region[0], region[1], region[2])).clamp(0.0, 1.0),
    )
}

/// True when any White Balance / Exposure / Light / Curve setting is engaged, i.e.
/// the tone stage (incl. the highlight roll-off) runs. The GPU preview needs this
/// to skip tone exactly like the CPU bake (which holds `tone = None` otherwise), so
/// a Colour-only edit does not roll off highlights on one side and not the other.
pub(crate) fn tone_is_active(settings: &DevelopSettings) -> bool {
    has_white_balance(settings)
        || settings.exposure.abs() > 0.001
        || settings.grade_shadow_strength.abs() > 0.001
        || settings.grade_highlight_strength.abs() > 0.001
        || has_light(settings)
        || has_curve(settings)
}

impl<'a> DevelopPlan<'a> {
    pub(crate) fn new(settings: &'a DevelopSettings, width: u32, height: u32) -> Self {
        let use_tone = tone_is_active(settings);
        Self {
            settings,
            inv_w: if width > 1 {
                1.0 / (width - 1) as f32
            } else {
                0.0
            },
            inv_h: if height > 1 {
                1.0 / (height - 1) as f32
            } else {
                0.0
            },
            tone: if use_tone {
                Some(build_tone_data(settings))
            } else {
                None
            },
            use_color: has_color(settings),
            use_effects: has_effects(settings),
            mixer_curves: build_mixer_curves_opt(settings),
            mixer_gated: mixer_edit_mask(settings).is_some(),
            locals: settings
                .locals
                .iter()
                .filter(|l| !l.settings.is_neutral())
                .map(|l| {
                    let s = l.settings.to_develop_settings();
                    let tone = tone_is_active(&s).then(|| build_tone_data(&s));
                    let use_color = has_color(&s);
                    let mixer_curves = build_mixer_curves_opt(&s);
                    LocalPlan {
                        shape: l.shape,
                        settings: s,
                        tone,
                        use_color,
                        mixer_curves,
                    }
                })
                .collect(),
        }
    }

    /// Local adjustments: each mask's tone/colour stage runs on the globally
    /// developed pixel and blends back by the mask weight. Runs LAST (after
    /// effects, before quantization) on every bake path; the live preview
    /// takes the CPU path while masks are active, so preview = commit by
    /// construction. Highlights/Shadows here use the per-pixel (global) tone
    /// form — no regional base — which is the classic local-brush behaviour.
    fn apply_locals(&self, rf: &mut f32, gf: &mut f32, bf: &mut f32, x: u32, y: u32) {
        if self.locals.is_empty() {
            return;
        }
        let nx = x as f32 * self.inv_w;
        let ny = y as f32 * self.inv_h;
        for lp in &self.locals {
            let m = lp.shape.weight(nx, ny);
            if m <= 0.003 {
                continue;
            }
            let (mut r2, mut g2, mut b2) = (*rf, *gf, *bf);
            if let Some(t) = &lp.tone {
                t.apply_interp(&mut r2, &mut g2, &mut b2);
                clamp_unit(&mut r2, &mut g2, &mut b2);
            }
            if lp.use_color {
                apply_color(
                    &lp.settings,
                    lp.mixer_curves.as_ref(),
                    &mut r2,
                    &mut g2,
                    &mut b2,
                );
                clamp_unit(&mut r2, &mut g2, &mut b2);
            }
            *rf += (r2 - *rf) * m;
            *gf += (g2 - *gf) * m;
            *bf += (b2 - *bf) * m;
        }
    }

    /// Post-tone regional base for the spatial effects. `base` is the regional
    /// luminance from `build_base_luma` (WB+Exposure already applied when tone
    /// is active); the tone curve is applied here so the effects compare a
    /// post-tone pixel with a post-tone base. Paths without a base (Texture or
    /// Vignette only) fall back to the pixel's own luma, which makes the
    /// spatial terms inert — the bake gating guarantees Clarity/Defog always
    /// arrive with a base.
    fn effects_base(&self, base: Option<f32>, r: f32, g: f32, b: f32) -> f32 {
        match base {
            Some(bl) => match &self.tone {
                Some(t) => t.map_luma_ref(bl),
                None => bl.clamp(0.0, 1.0),
            },
            None => luminance_f32(r, g, b).clamp(0.0, 1.0),
        }
    }

    /// One 8-bit pixel through tone → colour → effects. `base` is the regional
    /// base luminance (present when local-adaptation tone or spatial effects
    /// are active; the local tone path uses it for the Shadows/Highlights
    /// offset, the effects for local contrast / veil estimation).
    pub(crate) fn apply_pixel(
        &self,
        r: u8,
        g: u8,
        b: u8,
        a: u8,
        x: u32,
        y: u32,
        base: Option<f32>,
    ) -> [u8; 4] {
        let mut rf = r as f32 / 255.0;
        let mut gf = g as f32 / 255.0;
        let mut bf = b as f32 / 255.0;

        if let Some(tone) = &self.tone {
            match base {
                Some(bl) if tone.is_local => tone.apply_local(&mut rf, &mut gf, &mut bf, bl),
                _ => tone.apply(&mut rf, &mut gf, &mut bf),
            }
            clamp_unit(&mut rf, &mut gf, &mut bf);
        }

        if self.use_color {
            apply_color(
                self.settings,
                self.mixer_curves.as_ref(),
                &mut rf,
                &mut gf,
                &mut bf,
            );
            clamp_unit(&mut rf, &mut gf, &mut bf);
        }
        if self.use_effects {
            let eb = self.effects_base(base, rf, gf, bf);
            apply_effects(
                self.settings,
                &mut rf,
                &mut gf,
                &mut bf,
                x,
                y,
                self.inv_w,
                self.inv_h,
                eb,
            );
            clamp_unit(&mut rf, &mut gf, &mut bf);
        }
        self.apply_locals(&mut rf, &mut gf, &mut bf, x, y);

        [
            quantize_dither(rf, x, y, 0),
            quantize_dither(gf, x, y, 1),
            quantize_dither(bf, x, y, 2),
            a,
        ]
    }

    /// 16-bit mirror of [`Self::apply_pixel`]: the tone LUTs are interpolated
    /// and the result quantized to 16-bit (no dither needed), so a 16-bit
    /// document keeps continuous tone.
    fn apply_pixel16(
        &self,
        r: u16,
        g: u16,
        b: u16,
        a: u16,
        x: u32,
        y: u32,
        base: Option<f32>,
    ) -> [u16; 4] {
        let mut rf = r as f32 / 65535.0;
        let mut gf = g as f32 / 65535.0;
        let mut bf = b as f32 / 65535.0;

        if let Some(tone) = &self.tone {
            match base {
                Some(bl) if tone.is_local => tone.apply_local_interp(&mut rf, &mut gf, &mut bf, bl),
                _ => tone.apply_interp(&mut rf, &mut gf, &mut bf),
            }
            clamp_unit(&mut rf, &mut gf, &mut bf);
        }
        if self.use_color {
            apply_color(
                self.settings,
                self.mixer_curves.as_ref(),
                &mut rf,
                &mut gf,
                &mut bf,
            );
            clamp_unit(&mut rf, &mut gf, &mut bf);
        }
        if self.use_effects {
            let eb = self.effects_base(base, rf, gf, bf);
            apply_effects(
                self.settings,
                &mut rf,
                &mut gf,
                &mut bf,
                x,
                y,
                self.inv_w,
                self.inv_h,
                eb,
            );
            clamp_unit(&mut rf, &mut gf, &mut bf);
        }
        self.apply_locals(&mut rf, &mut gf, &mut bf, x, y);

        let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
        [q(rf), q(gf), q(bf), a]
    }

    /// Colour stage for one pixel. `toned` is the tone-mapped pixel; `region` is
    /// its edge-aware regional low-pass; `adjusted` is that region after the
    /// Color transform (both precomputed on the proxy). The pixel = adjusted
    /// region + re-added detail (luminance detail in full, chroma detail
    /// attenuated), so a strong boost lifts the whole region's colour/brightness
    /// — carrying off-hue specks with it — without amplifying blocky chroma or
    /// hard-edging patch boundaries. Returns dithered RGB.
    fn finish_colored_pixel_f32(
        &self,
        toned: [f32; 3],
        region: [f32; 3],
        adjusted: [f32; 3],
        x: u32,
        y: u32,
    ) -> [f32; 3] {
        let dr = toned[0] - region[0];
        let dg = toned[1] - region[1];
        let db = toned[2] - region[2];
        let dl = luminance_f32(dr, dg, db);
        // PER-PIXEL shadow/neutral fade: a dark or near-neutral PIXEL takes
        // little colour edit even inside a bright coloured region — so a thin
        // dark hair strand lying over skin is NOT lifted toward white by a skin
        // brightening; it keeps its own luma. This must read the pixel's OWN
        // value, not the region's: a thin strand averages out of the low-res
        // region (which reads as skin), so a region-based fade would lift it.
        // Chroma-rescued, so a dark COLOURED pixel (navy/burgundy/foliage) keeps
        // its edit while a dark near-neutral strand/rim fades out. The MASS
        // boundary between skin and the hair block stays smooth via the edge
        // suppression baked into the `adjusted` proxy, not this per-pixel term.
        let lt = luminance_f32(toned[0], toned[1], toned[2]).clamp(0.0, 1.0);
        let ct = rgb_chroma(toned[0], toned[1], toned[2]);
        let mut gate = smootherstep(0.0, 0.32, lt + ct * SHADOW_COLOR_RESCUE);
        // Mixer-band edits: gate by the REGION's band membership (smooth hue
        // selection), so the edit tapers across a colour boundary and cannot
        // recolour a neighbouring hue.
        let mixer_desat = self
            .settings
            .mixer_saturation
            .iter()
            .map(|v| (-*v / CONTROL_LIMIT).clamp(0.0, 1.0))
            .fold(0.0f32, f32::max);
        let mut mixer_affinity = 1.0;
        if self.mixer_gated {
            if let Some(curves) = &self.mixer_curves {
                mixer_affinity = if mixer_desat > 0.0 {
                    mixer_desat_affinity(curves, region)
                } else {
                    mixer_edit_affinity(curves, region)
                };
                gate *= mixer_affinity;
            }
        }
        // Attenuating chroma detail de-blocks JPEG chroma noise (a SMALL
        // deviation from the region). A BIG chroma deviation is a real colour
        // boundary — another object right next door — and halving it pulled
        // that pixel toward the boundary-mixed region colour: the visible
        // cross-boundary bleed. Keep grows to 1 with the deviation magnitude.
        let dev_mag = ((dr - dl).powi(2) + (dg - dl).powi(2) + (db - dl).powi(2)).sqrt();
        let keep =
            CHROMA_DETAIL_KEEP + (1.0 - CHROMA_DETAIL_KEEP) * smootherstep(0.08, 0.22, dev_mag);
        // A strong negative Saturation edit intentionally removes colour. Do
        // not reconstruct that same chroma from the full-resolution detail or
        // blue/aqua fringes survive around otherwise neutralised regions.
        let global_desat = (-self.settings.saturation / CONTROL_LIMIT).clamp(0.0, 1.0);
        let keep = keep * (1.0 - global_desat.max(mixer_desat * mixer_affinity));
        // Reconstruct the FULL adjusted region + re-added detail, then blend toward
        // the untouched pixel by the gate a SINGLE time. The old form pre-gated the
        // region→adjusted step as well, so a partial gate (dark or muted colours,
        // whose shadow/membership gate < 1) squared the edit — the per-pixel maths
        // was right but the live proxy result was nearly inert on shadows, foliage
        // and muted colour. Gate → 0 still returns the exact toned pixel (untouched
        // areas stay bit-faithful, no pull toward the low-res region).
        let recon = |adj: f32, dc: f32, t: f32| -> f32 {
            let full = (adj + dl + (dc - dl) * keep).clamp(0.0, 1.0);
            (t + (full - t) * gate).clamp(0.0, 1.0)
        };
        let mut rf = recon(adjusted[0], dr, toned[0].clamp(0.0, 1.0));
        let mut gf = recon(adjusted[1], dg, toned[1].clamp(0.0, 1.0));
        let mut bf = recon(adjusted[2], db, toned[2].clamp(0.0, 1.0));

        // Quality mode is the full-resolution direct colour transform. The
        // guided/proxy reconstruction is now an explicit optional smoothing
        // control instead of silently discarding half the chroma detail.
        let smoothing = (self.settings.color_smoothing / 100.0).clamp(0.0, 1.0);
        if smoothing < 1.0 {
            let (mut direct_r, mut direct_g, mut direct_b) = (toned[0], toned[1], toned[2]);
            apply_color(
                self.settings,
                self.mixer_curves.as_ref(),
                &mut direct_r,
                &mut direct_g,
                &mut direct_b,
            );
            rf = direct_r + (rf - direct_r) * smoothing;
            gf = direct_g + (gf - direct_g) * smoothing;
            bf = direct_b + (bf - direct_b) * smoothing;
            clamp_unit(&mut rf, &mut gf, &mut bf);
        }

        if self.use_effects {
            // The colour region IS a post-tone edge-aware low-pass, so its
            // luminance serves as the spatial effects' base on this path (the
            // GPU mirror reads dev_luma of the same uploaded region buffer).
            let eb = luminance_f32(region[0], region[1], region[2]).clamp(0.0, 1.0);
            apply_effects(
                self.settings,
                &mut rf,
                &mut gf,
                &mut bf,
                x,
                y,
                self.inv_w,
                self.inv_h,
                eb,
            );
            clamp_unit(&mut rf, &mut gf, &mut bf);
        }
        self.apply_locals(&mut rf, &mut gf, &mut bf, x, y);
        [rf, gf, bf]
    }

    fn finish_colored_pixel(
        &self,
        toned: [f32; 3],
        region: [f32; 3],
        adjusted: [f32; 3],
        x: u32,
        y: u32,
    ) -> [u8; 3] {
        let [rf, gf, bf] = self.finish_colored_pixel_f32(toned, region, adjusted, x, y);
        [
            quantize_dither(rf, x, y, 0),
            quantize_dither(gf, x, y, 1),
            quantize_dither(bf, x, y, 2),
        ]
    }

    /// 16-bit colour-path output (inputs are already full-precision f32).
    fn finish_colored_pixel16(
        &self,
        toned: [f32; 3],
        region: [f32; 3],
        adjusted: [f32; 3],
        x: u32,
        y: u32,
    ) -> [u16; 3] {
        let [rf, gf, bf] = self.finish_colored_pixel_f32(toned, region, adjusted, x, y);
        let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
        [q(rf), q(gf), q(bf)]
    }
}

/// Detail preservation for the local-adaptation lift, mirrored in the WGSL
/// local path. When the regional base moves by `offset`, headroom around it
/// changes by `gain = ratio of the moved side`; fine texture (small |l−base|)
/// is re-amplified by that ratio so it stays visible at the new brightness,
/// while big deviations (a bright strand inside a dark region is not "in" the
/// shadow) fade out of the boost and keep the plain additive behaviour.
const DETAIL_GAIN_EPS: f32 = 0.02;
pub(crate) const DETAIL_GAIN_MAX: f32 = 2.2;
/// Fraction of full Weber compensation applied (1.0 = keep perceived contrast
/// exactly constant). Full compensation reads over-crunchy and amplifies dark
/// noise; half-way matches the "lifted but still textured" look of the
/// commercial raw developers.
const DETAIL_BOOST_STRENGTH: f32 = 0.5;
const DETAIL_KEEP_LO: f32 = 0.10;
const DETAIL_KEEP_HI: f32 = 0.35;

#[inline]
fn local_detail_boost(l: f32, base: f32, offset: f32) -> f32 {
    if offset.abs() <= 1e-4 {
        return 0.0;
    }
    let base_new = (base + offset).clamp(0.0, 1.0);
    let ratio = if offset > 0.0 {
        (base_new + DETAIL_GAIN_EPS) / (base + DETAIL_GAIN_EPS)
    } else {
        (1.0 - base_new + DETAIL_GAIN_EPS) / (1.0 - base + DETAIL_GAIN_EPS)
    };
    let gain = 1.0 + (ratio.clamp(1.0, DETAIL_GAIN_MAX) - 1.0) * DETAIL_BOOST_STRENGTH;
    let d = l - base;
    let texture_w = 1.0 - smootherstep(DETAIL_KEEP_LO, DETAIL_KEEP_HI, d.abs());
    d * (gain - 1.0) * texture_w
}

/// Build the tone stage from the Light + Curve + White Balance + Exposure
/// settings. Called once per bake/preview (not per pixel).
pub(crate) fn build_tone_data(settings: &DevelopSettings) -> ToneData {
    let is_local = has_local_tone(settings);
    ToneData {
        gains: wb_gains(settings),
        ev: exposure_factor(settings.exposure),
        lut: build_tone_lut(settings),
        global_lut: if is_local {
            build_global_tone_lut(settings)
        } else {
            [0.0; 256]
        },
        local_lut: if is_local {
            build_local_tone_lut(settings)
        } else {
            [0.0; 256]
        },
        is_local,
        rgb: rgb_curve_luts(settings),
    }
}

fn has_local_tone(settings: &DevelopSettings) -> bool {
    settings.has_local_tone()
}

/// White balance as luma-preserving linear channel gains. The gain *vector* is
/// normalised once (Rec.709 luma → 1) so a neutral grey keeps its brightness and
/// only the colour cast moves — this is what makes WB foldable into a per-channel
/// table (no per-pixel renormalisation needed). Neutral settings → [1,1,1].
pub(crate) fn wb_gains(settings: &DevelopSettings) -> [f32; 3] {
    let temp = eased_control(settings.temperature);
    let tint = eased_control(settings.tint);
    let gr = (1.0 + 0.30 * temp) * (1.0 + 0.10 * tint);
    let gg = 1.0 - 0.20 * tint;
    let gb = (1.0 - 0.30 * temp) * (1.0 + 0.10 * tint);
    let l = luma_lin(gr, gg, gb);
    if l > 1e-6 {
        [gr / l, gg / l, gb / l]
    } else {
        [gr, gg, gb]
    }
}

/// Exposure as a true linear-light multiply. The slider is eased (powf 1.7) so
/// the working range around 0 moves gently — fine exposure drags feel comfortable
/// instead of jumping — while the extremes still reach the full ±MAX_EV stops.
fn exposure_factor(exposure: f32) -> f32 {
    const MAX_EV: f32 = 5.0;
    let t = (exposure / EXPOSURE_LIMIT).clamp(-1.0, 1.0);
    let eased = t.signum() * t.abs().powf(1.7);
    2.0_f32.powf(eased * MAX_EV)
}

/// Build the 256-entry unified tone curve over the perceptual (sRGB) axis.
///
/// Contrast supplies a monotone S-curve; Highlights/Shadows bend the upper/lower
/// regions; Whites/Blacks move the endpoints; the point-curve sliders add their
/// own region offsets. They are summed into ONE curve (not applied as four
/// sequential luma shifts like the old engine) and a final pass forces the table
/// to be non-decreasing — so the slider response is monotone and predictable and
/// tones never invert.
pub(crate) fn build_tone_lut(settings: &DevelopSettings) -> [f32; 256] {
    let contrast = eased_control(settings.contrast);
    let hi = eased_control(settings.highlights);
    let sh = eased_control(settings.shadows);
    let wh = eased_control(settings.whites);
    let bl = eased_control(settings.blacks);
    let ch = eased_control(settings.curve_highlights);
    let cl = eased_control(settings.curve_lights);
    let cd = eased_control(settings.curve_darks);
    let cs = eased_control(settings.curve_shadows);

    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let p0 = i as f32 / 255.0;
        let p = contrast_curve(p0, contrast * 1.05);
        let mut l = apply_light_luma(p, hi, sh, wh, bl);
        l += ch * 0.18 * smootherstep(0.64, 1.0, l);
        l += cl * 0.18 * bell(l, 0.62, 0.34);
        l += cd * 0.16 * darks_mask(l);
        l += cs * 0.14 * curve_shadows_mask(l);
        *slot = l.clamp(0.0, 1.0);
    }
    for i in 1..lut.len() {
        if lut[i] < lut[i - 1] {
            lut[i] = lut[i - 1];
        }
    }
    apply_point_curve_outer(&mut lut, settings);
    lut
}

/// The unified tone curve WITHOUT the Highlights/Shadows/Whites/Blacks region
/// offsets (those four are applied locally in `apply_local`). Everything else —
/// contrast and the point curve — is identical to `build_tone_lut`, including the
/// monotone pass.
fn build_global_tone_lut(settings: &DevelopSettings) -> [f32; 256] {
    let contrast = eased_control(settings.contrast);
    let ch = eased_control(settings.curve_highlights);
    let cl = eased_control(settings.curve_lights);
    let cd = eased_control(settings.curve_darks);
    let cs = eased_control(settings.curve_shadows);

    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let p0 = i as f32 / 255.0;
        let mut l = contrast_curve(p0, contrast * 1.05);
        l += ch * 0.18 * smootherstep(0.64, 1.0, l);
        l += cl * 0.18 * bell(l, 0.62, 0.34);
        l += cd * 0.16 * darks_mask(l);
        l += cs * 0.14 * curve_shadows_mask(l);
        *slot = l.clamp(0.0, 1.0);
    }
    for i in 1..lut.len() {
        if lut[i] < lut[i - 1] {
            lut[i] = lut[i - 1];
        }
    }
    apply_point_curve_outer(&mut lut, settings);
    lut
}

/// The Highlights/Shadows AND Whites/Blacks contribution, as a signed luma OFFSET
/// indexed by luminance. `apply_local` reads this at the *regional* (blurred) luma
/// and adds it to `global_lut` read at the pixel luma — so a dark (or bright)
/// region is moved uniformly, preserving local contrast (texture, fine strands).
/// Blacks/Whites are endpoint moves, but reading them regionally is what stops a
/// Blacks pull from crushing every dark pixel to one value; a bright strand inside
/// a dark region keeps a bright regional luma (edge-aware base) so it is spared.
/// The masks match `build_tone_lut` so a flat region reproduces global behaviour.
fn build_local_tone_lut(settings: &DevelopSettings) -> [f32; 256] {
    let contrast = eased_control(settings.contrast);
    let hi = eased_control(settings.highlights);
    let sh = eased_control(settings.shadows);
    let wh = eased_control(settings.whites);
    let bl = eased_control(settings.blacks);

    let mut lut = [0.0f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let p = contrast_curve(i as f32 / 255.0, contrast * 1.05);
        *slot = apply_light_luma(p, hi, sh, wh, bl) - p;
    }
    lut
}

fn has_white_balance(settings: &DevelopSettings) -> bool {
    settings.temperature.abs() > 0.001 || settings.tint.abs() > 0.001
}

fn has_light(settings: &DevelopSettings) -> bool {
    settings.contrast.abs() > 0.001
        || settings.highlights.abs() > 0.001
        || settings.shadows.abs() > 0.001
        || settings.whites.abs() > 0.001
        || settings.blacks.abs() > 0.001
}

pub(crate) fn has_color(settings: &DevelopSettings) -> bool {
    settings.vibrance.abs() > 0.001
        || settings.saturation.abs() > 0.001
        || settings.mixer_hue.iter().any(|v| v.abs() > 0.001)
        || settings.mixer_saturation.iter().any(|v| v.abs() > 0.001)
        || settings.mixer_luminance.iter().any(|v| v.abs() > 0.001)
}

fn has_curve(settings: &DevelopSettings) -> bool {
    settings.curve_highlights.abs() > 0.001
        || settings.curve_lights.abs() > 0.001
        || settings.curve_darks.abs() > 0.001
        || settings.curve_shadows.abs() > 0.001
        || !curve_is_identity(&settings.curve_points)
        || !curve_is_identity(&settings.curve_points_r)
        || !curve_is_identity(&settings.curve_points_g)
        || !curve_is_identity(&settings.curve_points_b)
}

pub(crate) fn has_effects(settings: &DevelopSettings) -> bool {
    settings.texture.abs() > 0.001
        || settings.clarity.abs() > 0.001
        || settings.dehaze.abs() > 0.001
        || settings.vignette.abs() > 0.001
}

pub(crate) fn has_detail(settings: &DevelopSettings) -> bool {
    settings.sharpening.abs() > 0.001
        || settings.noise_reduction.abs() > 0.001
        || settings.color_noise_reduction.abs() > 0.001
}
