//! Colour mode and depth: CMYK ink planes and conversions, working-profile
//! assignment/conversion, grayscale, 8/16-bit flat exports.

// Main document model — Canvas, History, metadata.
//

use super::*;
use crate::core::gateway::ChangeKind;

pub use crate::core::layer::BlendMode;

impl Canvas {
    #[inline]
    pub fn is_cmyk(&self) -> bool {
        matches!(self.color_mode, ColorMode::Cmyk(_))
    }

    /// The ink↔RGB converter of a CMYK document (`None` in RGB mode or if the
    /// document's ICC payload is unusable). Build once per stroke/operation and
    /// reuse — the ICC variant holds prebuilt lcms2 transforms.
    pub fn cmyk_converter(&self) -> Option<crate::core::cms::CmykConverter> {
        match &self.color_mode {
            ColorMode::Rgb => None,
            ColorMode::Cmyk(profile) => profile.converter(),
        }
    }

    /// Flatten the document's ink planes onto white paper: per-channel
    /// source-over with each pixel's mirror alpha × layer opacity (the same
    /// ink-space compositing model the CMYK brush uses). Returns a packed
    /// CMYK8 buffer (4 bytes/pixel, 255 = full ink), or `None` when the stack
    /// is not ink-exact — a group, a non-Normal blend mode, an enabled mask,
    /// an adjustment layer, or a painted pixel without ink all disqualify it.
    /// Callers then fall back to converting the flattened RGB mirror.
    pub fn flatten_ink(&self) -> Option<Vec<u8>> {
        use crate::core::tile::TILE_SIZE;
        if !self.is_cmyk() {
            return None;
        }
        // Groups composite in RGB (isolation, group opacity); refuse outright.
        if self
            .layer_stack
            .layers
            .iter()
            .any(|l| l.is_group() || l.parent_id.is_some())
        {
            return None;
        }
        let len = Self::checked_rgba_len(self.width, self.height)?;
        let mut out = vec![0u8; len];
        for layer in &self.layer_stack.layers {
            if !layer.visible {
                continue;
            }
            if matches!(
                layer.layer_type,
                crate::core::layer::LayerType::Adjustment(_)
            ) {
                return None;
            }
            if layer.blend_mode != BlendMode::Normal {
                return None;
            }
            if layer.mask.as_ref().is_some_and(|m| m.enabled) {
                return None;
            }
            let op = layer.opacity.clamp(0.0, 1.0);
            if op <= 0.0 {
                continue;
            }
            for (pos, tile) in &layer.tiles.tiles {
                let base_x = layer.offset.0 as i64 + pos.x as i64 * TILE_SIZE as i64;
                let base_y = layer.offset.1 as i64 + pos.y as i64 * TILE_SIZE as i64;
                for p in 0..(TILE_SIZE * TILE_SIZE) as usize {
                    let i = p * 4;
                    let a = tile.pixels[i + 3];
                    if a == 0 {
                        continue;
                    }
                    // A painted pixel without an ink plane means some edit
                    // bypassed the ink path — the plates would lie.
                    let ink = tile.ink.as_ref()?;
                    let cx = base_x + (p as u32 % TILE_SIZE) as i64;
                    let cy = base_y + (p as u32 / TILE_SIZE) as i64;
                    if cx < 0 || cy < 0 || cx >= self.width as i64 || cy >= self.height as i64 {
                        continue;
                    }
                    let eff = a as f32 / 255.0 * op;
                    if eff <= 0.0 {
                        continue;
                    }
                    let o = ((cy as u64 * self.width as u64 + cx as u64) * 4) as usize;
                    for c in 0..4 {
                        out[o + c] = (ink[i + c] as f32 * eff + out[o + c] as f32 * (1.0 - eff))
                            .round() as u8;
                    }
                }
            }
        }
        Some(out)
    }

    /// Build a grayscale preview of one CMYK ink plate for the Channels panel.
    ///
    /// The returned buffer is one byte per canvas pixel. It is display-oriented:
    /// paper/no ink is white (255), full ink is black (0). This mirrors how print
    /// plates are inspected and lets the existing alpha-plane GPU override show
    /// CMYK channels without teaching the blit shader about ink planes.
    pub fn cmyk_plate_preview_mask(&self, channel: u8) -> Option<Vec<u8>> {
        let channel = channel as usize;
        if !self.is_cmyk() || channel >= 4 {
            return None;
        }
        let len =
            Self::pixel_count(self.width, self.height).and_then(|n| usize::try_from(n).ok())?;
        let mut density = vec![0.0_f32; len];
        let canvas_w = self.width as i32;
        let canvas_h = self.height as i32;
        let tile_size = crate::core::tile::TILE_SIZE as i32;

        for (layer_idx, layer) in self.layer_stack.layers.iter().enumerate() {
            if layer.is_group()
                || !layer.is_raster()
                || !self.layer_stack.is_effectively_visible(layer_idx)
            {
                continue;
            }
            let opacity = layer.opacity.clamp(0.0, 1.0);
            if opacity < 0.001 {
                continue;
            }
            let mask = layer.mask.as_ref().filter(|m| m.enabled);
            let (ox, oy) = layer.offset;
            let layer_w = layer.width as i32;
            let layer_h = layer.height as i32;

            for (pos, tile) in layer.tiles.tiles.iter() {
                let Some(ink) = tile.ink.as_ref() else {
                    continue;
                };
                let tile_x0 = pos.x * tile_size;
                let tile_y0 = pos.y * tile_size;

                let lx0 = tile_x0.max(0).max(-ox);
                let ly0 = tile_y0.max(0).max(-oy);
                let lx1 = (tile_x0 + tile_size).min(layer_w).min(canvas_w - ox);
                let ly1 = (tile_y0 + tile_size).min(layer_h).min(canvas_h - oy);
                if lx1 <= lx0 || ly1 <= ly0 {
                    continue;
                }

                for ly in ly0..ly1 {
                    let cy = ly + oy;
                    let tile_y = ly - tile_y0;
                    let dst_row = cy as usize * self.width as usize;
                    for lx in lx0..lx1 {
                        let cx = lx + ox;
                        let tile_x = lx - tile_x0;
                        let ti = ((tile_y * tile_size + tile_x) * 4) as usize;
                        if ti + 3 >= tile.pixels.len() || ti + channel >= ink.len() {
                            continue;
                        }
                        let mut sa = tile.pixels[ti + 3] as f32 / 255.0 * opacity;
                        if let Some(mask) = mask {
                            sa *= mask.sample(lx as u32, ly as u32);
                        }
                        if sa < 0.001 {
                            continue;
                        }
                        let src = ink[ti + channel] as f32 / 255.0;
                        let di = dst_row + cx as usize;
                        density[di] = src * sa + density[di] * (1.0 - sa);
                    }
                }
            }
        }

        Some(
            density
                .into_iter()
                .map(|v| ((1.0 - v.clamp(0.0, 1.0)) * 255.0).round() as u8)
                .collect(),
        )
    }

    /// Flatten the whole visible layer stack to native 16-bit RGBA samples
    /// (`width*height*4`) for export. Returns `None` for an 8-bit document (the
    /// caller falls back to the 8-bit export). Every layer's offset, opacity,
    /// mask and blend mode is composited at full 16-bit precision via
    /// [`LayerStack::flatten16`].
    pub fn export_flat16_samples(&self) -> Option<Vec<u16>> {
        if self.bit_depth != BitDepth::Sixteen {
            return None;
        }
        let flat = self.layer_stack.flatten16(self.width, self.height);
        if flat.is_empty() {
            None
        } else {
            Some(flat)
        }
    }

    /// Same as [`Self::export_flat16_samples`] but packed to big-endian bytes
    /// (PNG 16-bit sample order).
    pub fn export_flat16(&self) -> Option<Vec<u8>> {
        let flat = self.export_flat16_samples()?;
        let mut out = vec![0u8; flat.len() * 2];
        for (i, &v) in flat.iter().enumerate() {
            out[i * 2] = (v >> 8) as u8;
            out[i * 2 + 1] = (v & 0xFF) as u8;
        }
        Some(out)
    }

    /// Switch the document bit depth (Image ▸ Mode). 8→16 up-converts every
    /// layer's tiles to a 16-bit master; 16→8 drops the masters. No-op if already
    /// at the requested depth.
    pub fn set_bit_depth(&mut self, sixteen: bool) {
        let target = if sixteen {
            BitDepth::Sixteen
        } else {
            BitDepth::Eight
        };
        if self.bit_depth == target {
            return;
        }
        for layer in &mut self.layer_stack.layers {
            if sixteen {
                layer.tiles.promote_to_hdr();
            } else {
                layer.tiles.drop_hdr();
            }
        }
        self.bit_depth = target;
    }

    pub fn export_flat(&self) -> Vec<u8> {
        if Self::fits_flat_buffer(self.width, self.height) {
            self.layer_stack.flatten(self.width, self.height)
        } else {
            self.pixels.clone()
        }
    }

    /// Flatten for an explicit output operation with a caller-provided safety
    /// limit. Large canvases intentionally keep no always-resident flat buffer,
    /// but PDF export still needs to materialize one page at a time.
    pub fn export_flat_up_to(&self, max_pixels: u64) -> Option<Vec<u8>> {
        let pixels = Self::pixel_count(self.width, self.height)?;
        if pixels == 0 || pixels > max_pixels {
            return None;
        }
        Some(self.layer_stack.flatten(self.width, self.height))
    }

    /// After a subset merge (Merge Down / Merge Selected) on a 16-bit document,
    /// re-promote every layer's tiles so the freshly merged 8-bit tiles regain a
    /// 16-bit master and the document stays uniformly 16-bit. The merged pixels
    /// carry 8-bit precision (v*257); full-precision subset merging is future
    /// work. Flatten Image / Stamp Visible use the true 16-bit composite instead.
    pub(crate) fn ensure_16bit_layer_masters(&mut self) {
        if self.bit_depth == BitDepth::Sixteen {
            for layer in &mut self.layer_stack.layers {
                layer.tiles.promote_to_hdr();
            }
        }
    }

    /// Re-interpret every raster layer as having been authored in the `from`
    /// profile and bake it into the sRGB working space (Image ▸ Assign Profile).
    /// Fixes mis-tagged / untagged imports. Undoable via a full layer snapshot;
    /// the document is then tagged sRGB. No-op result when `from` is sRGB.
    pub fn assign_profile(&mut self, from: crate::core::cms::WorkingProfile) {
        use crate::core::cms;
        use crate::core::command::LayerStructureCommand;
        use crate::core::tile::TileMap;

        let mut cmd = LayerStructureCommand::capture_before(
            "Assign Profile",
            &self.layer_stack,
            self.width,
            self.height,
        );

        let src = from.profile();
        let dst = cms::srgb_profile();
        for layer in &mut self.layer_stack.layers {
            // Only raster layers hold pixel tiles; adjustment/group layers are empty.
            if layer.tiles.tiles.is_empty() {
                continue;
            }
            let (w, h) = (layer.width, layer.height);
            let mut flat = layer.tiles.flatten();
            if cms::convert_rgba8(&mut flat, &src, &dst, cms::DEFAULT_INTENT) {
                layer.tiles = TileMap::from_rgba(&flat, w, h);
            }
        }

        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);

        self.icc_profile = IccProfile {
            name: cms::WorkingProfile::Srgb.name().to_string(),
            data: cms::srgb_icc_bytes(),
        };
        self.metadata.source_profile = from.name().to_string();
        self.flatten_full();
    }

    /// Image ▸ Mode ▸ Grayscale — desaturate every raster layer to its Rec.709
    /// luminance (r=g=b=luma, alpha kept), as one undoable step. The document stays
    /// RGB internally (no channel-count change), so colour can still be painted back;
    /// this matches the visible result of a Grayscale-mode conversion.
    pub fn convert_to_grayscale(&mut self) {
        use crate::core::color::luminance_f32;
        use crate::core::command::LayerStructureCommand;
        use crate::core::tile::TileMap;

        let mut cmd = LayerStructureCommand::capture_before(
            "Grayscale",
            &self.layer_stack,
            self.width,
            self.height,
        );
        for layer in &mut self.layer_stack.layers {
            if layer.tiles.tiles.is_empty() {
                continue;
            }
            let (w, h) = (layer.width, layer.height);
            let mut flat = layer.tiles.flatten();
            flat.chunks_exact_mut(4).for_each(|px| {
                let l = luminance_f32(
                    px[0] as f32 / 255.0,
                    px[1] as f32 / 255.0,
                    px[2] as f32 / 255.0,
                )
                .clamp(0.0, 1.0);
                let g = (l * 255.0).round() as u8;
                px[0] = g;
                px[1] = g;
                px[2] = g;
            });
            layer.tiles = TileMap::from_rgba(&flat, w, h);
        }
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);
        self.flatten_full();
    }

    /// Image ▸ Color Profile ▸ Convert to Profile — transform every raster layer's
    /// pixels from the document's current profile into `target` (unlike Assign, which
    /// only re-tags), then tag the document as `target`. 8-bit path (like
    /// `assign_profile`).
    pub fn convert_to_profile(&mut self, target: crate::core::cms::WorkingProfile) {
        use crate::core::cms;
        use crate::core::command::LayerStructureCommand;
        use crate::core::tile::TileMap;

        let src = if !self.icc_profile.data.is_empty() {
            cms::profile_from_bytes(&self.icc_profile.data).unwrap_or_else(cms::srgb_profile)
        } else {
            cms::srgb_profile()
        };
        let dst = target.profile();

        let mut cmd = LayerStructureCommand::capture_before(
            "Convert to Profile",
            &self.layer_stack,
            self.width,
            self.height,
        );
        for layer in &mut self.layer_stack.layers {
            if layer.tiles.tiles.is_empty() {
                continue;
            }
            let (w, h) = (layer.width, layer.height);
            let mut flat = layer.tiles.flatten();
            if cms::convert_rgba8(&mut flat, &src, &dst, cms::DEFAULT_INTENT) {
                layer.tiles = TileMap::from_rgba(&flat, w, h);
            }
        }
        cmd.capture_after(&self.layer_stack, self.width, self.height);
        self.record_as(Box::new(cmd), ChangeKind::LayerStructure);

        self.icc_profile = IccProfile {
            name: target.name().to_string(),
            data: cms::icc_bytes(&dst),
        };
        self.color_space = match target {
            cms::WorkingProfile::Srgb => ColorSpace::SRGB,
            cms::WorkingProfile::AdobeRgb => ColorSpace::AdobeRGB,
        };
        self.metadata.source_profile = target.name().to_string();
        self.flatten_full();
    }

    /// Image ▸ Mode ▸ CMYK Color: flatten the document, encode every tile's ink
    /// plane from its RGB content through `profile`, re-project the mirrors, and
    /// switch the mode. Destructive by design (v1): layers are flattened, 16-bit
    /// masters drop to 8-bit, and the undo history is CLEARED (old snapshots
    /// hold ink-less tiles that would desync the document if restored) — the UI
    /// warns before calling this. Errors leave the document untouched.
    pub fn convert_to_cmyk(&mut self, profile: CmykProfile) -> Result<(), String> {
        if self.is_cmyk() {
            return Ok(());
        }
        let conv = profile
            .converter()
            .ok_or_else(|| "Invalid CMYK profile or the ICC file is not CMYK".to_string())?;

        if self.bit_depth == BitDepth::Sixteen {
            self.layer_stack.merge_all16(self.width, self.height);
            self.bit_depth = BitDepth::Eight;
            for layer in &mut self.layer_stack.layers {
                layer.tiles.drop_hdr();
            }
        } else {
            self.layer_stack.merge_all(self.width, self.height);
        }

        for layer in &mut self.layer_stack.layers {
            layer.tiles.encode_ink_from_mirror(&conv);
        }

        self.color_mode = ColorMode::Cmyk(profile);
        self.channels.select_composite();
        self.pending_stroke = None;
        self.reset_history();
        self.flatten_full();
        Ok(())
    }

    /// Image ▸ Mode ▸ RGB Color (from CMYK): drop the ink planes — the RGB
    /// mirror, being the profile projection of that ink, becomes the ground
    /// truth. History is cleared for the same snapshot-desync reason as
    /// [`Self::convert_to_cmyk`].
    pub fn convert_to_rgb_mode(&mut self) {
        if !self.is_cmyk() {
            return;
        }
        for layer in &mut self.layer_stack.layers {
            layer.tiles.drop_ink();
        }
        self.color_mode = ColorMode::Rgb;
        self.channels.select_composite();
        self.pending_stroke = None;
        self.reset_history();
        self.flatten_full();
    }
}
