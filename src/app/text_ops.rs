// Text tool flow: create / edit a text layer, drive the egui editing overlay,
// and rasterize the text into the layer on commit. The tool itself
// (tools/text_tool.rs) is a stub; all the real work happens here at App level.

use super::render::CanvasEvent;
use super::state::{App, TextEditState, TextFontPreviewSession, TextFontPreviewState};
use crate::core::canvas::Canvas;
use crate::core::command::LayerStructureCommand;
use crate::core::layer::{Layer, LayerType};
use crate::core::text::{rasterize_placed, TextData};
use crate::core::tile::TileMap;
use crate::tools::ToolId;

/// Rasterize `td` and replace the layer's tiles with it. The text is written at
/// tile-local (0,0); `origin` is the upright text anchor, and rotated tiles
/// receive a bbox offset. Also updates the layer name from the content.
///
/// The tiles are raster-sized (not canvas-sized): a canvas-sized TileMap would
/// silently crop text extending past the canvas edge, and the transform-commit
/// path already produces raster-sized text layers — width/height must track
/// the tiles for hit-testing and .iai export.
fn rasterize_into_layer(canvas: &mut Canvas, idx: usize, origin: (i32, i32), td: TextData) {
    let (cw, ch) = (canvas.width, canvas.height);
    let (tiles, w, h, offset) = match rasterize_placed(&td) {
        Some((raster, delta)) => (
            TileMap::from_rgba(&raster.rgba, raster.width, raster.height),
            raster.width,
            raster.height,
            (
                origin.0.saturating_add(delta.0),
                origin.1.saturating_add(delta.1),
            ),
        ),
        None => (TileMap::new(cw, ch), cw, ch, origin),
    };
    let name = layer_name_from(&td.content);
    let layer = &mut canvas.layer_stack.layers[idx];
    layer.tiles = tiles;
    layer.width = w;
    layer.height = h;
    layer.offset = offset;
    layer.name = name;
    layer.layer_type = LayerType::Text(td);
}

fn text_edit_origin_for_layer(td: &TextData, layer: &Layer) -> (i32, i32) {
    let Some((placed, delta)) = rasterize_placed(td) else {
        return layer.offset;
    };
    let placed_tiles = TileMap::from_rgba(&placed.rgba, placed.width, placed.height);
    let Some((placed_min_x, placed_min_y, _, _)) = placed_tiles.content_bounds() else {
        return layer.offset;
    };
    let Some((layer_min_x, layer_min_y, _, _)) = layer.tiles.content_bounds() else {
        return layer.offset;
    };

    (
        layer
            .offset
            .0
            .saturating_add(layer_min_x)
            .saturating_sub(delta.0)
            .saturating_sub(placed_min_x),
        layer
            .offset
            .1
            .saturating_add(layer_min_y)
            .saturating_sub(delta.1)
            .saturating_sub(placed_min_y),
    )
}

/// Origin for re-fonting into `layer` that keeps the block's ink centre fixed
/// on the canvas, so text centred inside a frame stays centred when the new
/// font changes its width/height. Works for rotated/stretched text too: both
/// centres are measured on the *placed* raster and `delta` maps back to the
/// layer offset. Returns `None` when either side has no ink (blank), so the
/// caller can fall back to the plain corner anchor.
fn compute_refont_origin(layer: &Layer, new_td: &TextData) -> Option<(i32, i32)> {
    let old_bounds = layer.tiles.content_bounds()?;
    let (raster, delta) = rasterize_placed(new_td)?;
    let new_tiles = TileMap::from_rgba(&raster.rgba, raster.width, raster.height);
    let new_bounds = new_tiles.content_bounds()?;
    Some(refont_origin_from_bounds(
        layer.offset,
        old_bounds,
        new_bounds,
        delta,
    ))
}

/// Pure centre-anchor arithmetic: the origin at which a new placed raster whose
/// ink spans `new_bounds` must be drawn so its ink centre coincides with the
/// ink centre of the current tiles (`old_bounds` at `offset`). `delta` is the
/// placed raster's bbox offset, which `rasterize_into_layer` re-adds to the
/// origin.
fn refont_origin_from_bounds(
    offset: (i32, i32),
    old_bounds: (i32, i32, i32, i32),
    new_bounds: (i32, i32, i32, i32),
    delta: (i32, i32),
) -> (i32, i32) {
    let old_cx = offset.0 as f32 + (old_bounds.0 + old_bounds.2) as f32 * 0.5;
    let old_cy = offset.1 as f32 + (old_bounds.1 + old_bounds.3) as f32 * 0.5;
    let new_cx = (new_bounds.0 + new_bounds.2) as f32 * 0.5;
    let new_cy = (new_bounds.1 + new_bounds.3) as f32 * 0.5;
    (
        (old_cx - new_cx).round() as i32 - delta.0,
        (old_cy - new_cy).round() as i32 - delta.1,
    )
}

/// Default font size that reads well on a canvas of the given dimensions
/// (~4% of the smaller side): 12pt-style constants are invisible on print-res
/// documents and huge on thumbnails.
fn auto_font_px(cw: u32, ch: u32) -> f32 {
    ((cw.min(ch) as f32) * 0.04).round().clamp(10.0, 256.0)
}

/// Human-readable label for a font family in a list (family name, plus the
/// style when it isn't the plain Regular face).
fn font_display_label(fam: &crate::core::text::TextFontFamily) -> String {
    let name = fam.name();
    let style = fam.style_name();
    if style.eq_ignore_ascii_case("Regular") {
        name.to_string()
    } else {
        format!("{name} {style}")
    }
}

/// First line of the text, trimmed and truncated, for the layer panel name.
fn layer_name_from(content: &str) -> String {
    let first = content.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return "Text".to_string();
    }
    let truncated: String = first.chars().take(24).collect();
    truncated
}

/// Rebuild `session.glyph_styles` after the buffer changed from `old` to the
/// new buffer, using a prefix/suffix diff to find the inserted/removed span.
/// Inserted characters take the pending style (if the insertion is at the
/// pending caret), else inherit the neighbouring character, else `base`.
fn resync_glyph_styles(
    session: &mut TextEditState,
    old: &str,
    base: crate::core::text::GlyphStyle,
) {
    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = session.buffer.chars().collect();

    // Nothing per-character to track and no pending style → stay uniform.
    if session.glyph_styles.is_empty() && session.pending_style.is_none() {
        return;
    }

    let mut p = 0usize;
    while p < old_chars.len() && p < new_chars.len() && old_chars[p] == new_chars[p] {
        p += 1;
    }
    let (mut so, mut sn) = (old_chars.len(), new_chars.len());
    while so > p && sn > p && old_chars[so - 1] == new_chars[sn - 1] {
        so -= 1;
        sn -= 1;
    }

    // The pending style is consumed by an insertion at its caret and survives
    // pure deletions (caret shifted with the removed span) — a Backspace
    // before typing must not lose it. Any other edit discards it.
    let inserted = sn > p;
    let pending = match session.pending_style.take() {
        Some((caret, style)) if inserted && caret == p => Some((caret, style)),
        Some((caret, style)) if !inserted && so > p => {
            let shifted = if caret >= so {
                caret - (so - p)
            } else {
                caret.min(p)
            };
            session.pending_style = Some((shifted, style));
            None
        }
        _ => None,
    };

    // Materialize old styles to full length so we can splice.
    let old_len = old_chars.len();
    let old_styles: Vec<crate::core::text::GlyphStyle> = if session.glyph_styles.len() == old_len {
        session.glyph_styles.clone()
    } else {
        vec![base.clone(); old_len]
    };

    let seed = || -> crate::core::text::GlyphStyle {
        if let Some((caret, style)) = pending.as_ref() {
            if *caret == p {
                return style.clone();
            }
        }
        if p > 0 {
            old_styles[p - 1].clone()
        } else if so < old_len {
            old_styles[so].clone()
        } else {
            base.clone()
        }
    };

    let mut out = Vec::with_capacity(new_chars.len());
    out.extend_from_slice(&old_styles[..p]);
    for _ in p..sn {
        out.push(seed());
    }
    out.extend_from_slice(&old_styles[so..old_len]);
    session.glyph_styles = out;
}

impl App {
    /// Convert the Text layer at `idx` into editable vector Path layers ("Convert
    /// Text to Curves"). Each distinct fill colour becomes one Path layer: the
    /// first reuses the text layer's slot IN PLACE (keeping its id / stacking order
    /// / opacity / blend / group / page — mirroring `convert_shape_to_path`), any
    /// extra colours are inserted just above, inheriting the text layer's
    /// compositing. Every glyph outline lands exactly where the rasterized text
    /// sat. One structural undo restores the Text layer. Returns true when it
    /// converted.
    ///
    /// The existing layer mask is preserved on the in-place Path layer, just like
    /// Shape-to-Curves; the vector opacity carries the layer's own
    /// `Layer::opacity`, while `TextData::opacity` is folded into each object's
    /// `VectorStyle::opacity` by [`crate::core::text::text_to_curves`].
    pub fn text_to_curves(&mut self, idx: usize) -> bool {
        use crate::core::command_vector::apply_object_to_layer;
        use crate::core::vector::affine::AffineTransform;
        use crate::core::vector::color::ColorValue;
        use crate::core::vector::object::VectorObjectData;
        use crate::core::vector::style::VectorStyle;

        // Snapshot the text layer's data + the properties extra colour layers
        // inherit.
        let (td, layer_offset, blend, layer_opacity, visible, parent_id, page_id) = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            let Some(layer) = canvas.layer_stack.layers.get(idx) else {
                return false;
            };
            let LayerType::Text(td) = &layer.layer_type else {
                return false;
            };
            (
                td.clone(),
                layer.offset,
                layer.blend_mode,
                layer.opacity,
                layer.visible,
                layer.parent_id,
                layer.page_id,
            )
        };
        let td_opacity = td.opacity.clamp(0.0, 1.0);

        // Build one validated vector object per colour group (skip any the parser
        // limits would reject, so a pathological glyph count can't poison a file).
        let mut objects: Vec<VectorObjectData> = Vec::new();
        for grp in crate::core::text::text_to_curves(&td, layer_offset) {
            let style = VectorStyle {
                opacity: td_opacity,
                ..VectorStyle::filled(ColorValue::from_rgba8(grp.color))
            };
            let obj = VectorObjectData::new(grp.path, style, AffineTransform::IDENTITY);
            if obj.validate().is_ok() {
                objects.push(obj);
            }
        }
        if objects.is_empty() {
            self.shell.status_msg = "No text to convert to curves".to_string();
            return false;
        }

        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let before =
            LayerStructureCommand::capture_before("Text to Curves", &canvas.layer_stack, cw, ch);

        // First colour reuses the text layer slot in place.
        {
            let Some(layer) = canvas.layer_stack.layers.get_mut(idx) else {
                return false;
            };
            apply_object_to_layer(layer, objects[0].clone());
        }
        canvas.layer_stack.active_idx = idx;

        // Extra colours: fresh Path layers just above, inheriting the text layer's
        // compositing so the stack looks identical.
        for obj in objects.iter().skip(1) {
            let new_idx = canvas.layer_stack.add_layer(cw, ch);
            let layer = &mut canvas.layer_stack.layers[new_idx];
            apply_object_to_layer(layer, obj.clone());
            layer.blend_mode = blend;
            layer.opacity = layer_opacity;
            layer.visible = visible;
            layer.parent_id = parent_id;
            layer.page_id = page_id;
            layer.name = layer_name_from(&td.content);
        }

        // CMYK: re-derive ink planes for the new Path rasters from their RGB mirror.
        canvas.reconcile_path_ink();
        canvas.layer_revision += 1;

        // Select the converted layer so the Move box shows without an extra click.
        // Do this before capturing the after-state so redo restores the exact
        // selection produced by the original conversion.
        for l in &mut canvas.layer_stack.layers {
            l.selected = false;
        }
        let active = canvas
            .layer_stack
            .active_idx
            .min(canvas.layer_stack.layers.len().saturating_sub(1));
        if let Some(l) = canvas.layer_stack.layers.get_mut(active) {
            l.selected = true;
        }

        let mut cmd = before;
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.shell.status_msg = "Converted text to curves (Path)".to_string();
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// True when the active layer is an editable Text layer.
    pub fn active_layer_is_text(&self) -> bool {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        canvas
            .layer_stack
            .layers
            .get(canvas.layer_stack.active_idx)
            .is_some_and(|l| matches!(l.layer_type, LayerType::Text(_)))
    }

    /// Fill a Text layer's colour (Move-tool Alt/Ctrl+Delete): recolour the
    /// whole type to `color`, overriding any per-glyph colours, then re-rasterize
    /// in place so the layer stays an editable Text layer. Returns true when it
    /// recoloured; recorded as one undo step.
    pub fn recolor_text_layer(&mut self, idx: usize, color: [u8; 4]) -> bool {
        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let (mut new_td, origin) = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            let Some(layer) = canvas.layer_stack.layers.get(idx) else {
                return false;
            };
            if layer.locked && !layer.is_background {
                return false;
            }
            let LayerType::Text(td) = &layer.layer_type else {
                return false;
            };
            // Already that colour everywhere → nothing to do.
            if td.color == color && td.glyph_styles.iter().all(|g| g.color == color) {
                return false;
            }
            let origin = text_edit_origin_for_layer(td, layer);
            (td.clone(), origin)
        };
        new_td.color = color;
        for g in &mut new_td.glyph_styles {
            g.color = color;
        }

        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let before =
            LayerStructureCommand::capture_before("Fill Text Colour", &canvas.layer_stack, cw, ch);
        rasterize_into_layer(canvas, idx, origin, new_td);
        canvas.layer_revision += 1;
        let mut cmd = before;
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        true
    }

    /// Distinct fonts currently used across the active document's Text layers
    /// (each layer's base font plus any per-glyph overrides), returned as
    /// `(storage_name, display_label)` sorted by display label. Feeds the batch
    /// "Change font" dialog's *from* list so the user picks among fonts that are
    /// actually present.
    pub fn document_text_fonts_in_use(&self) -> Vec<(String, String)> {
        use std::collections::BTreeMap;
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        for layer in &canvas.layer_stack.layers {
            if let LayerType::Text(td) = &layer.layer_type {
                map.entry(td.font_family.storage_name())
                    .or_insert_with(|| font_display_label(&td.font_family));
                for g in &td.glyph_styles {
                    map.entry(g.font_family.storage_name())
                        .or_insert_with(|| font_display_label(&g.font_family));
                }
            }
        }
        let mut out: Vec<(String, String)> = map.into_iter().collect();
        out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        out
    }

    /// Batch-change the font of every editable Text layer in the active document
    /// (all pages). `from == None` switches every text layer to `to`;
    /// `from == Some(storage)` switches only the glyphs whose family
    /// `storage_name()` equals `storage` — base font and per-character overrides
    /// alike — leaving text set to other fonts untouched. Locked (non-background)
    /// layers are skipped.
    ///
    /// Each changed layer is re-rasterized in place, keeping its on-canvas
    /// position exactly as a manual open→pick font→commit would (origin recovered
    /// from the *original* text via [`text_edit_origin_for_layer`]). The whole
    /// batch is a single undo step. Returns how many layers changed.
    pub fn change_document_text_font(
        &mut self,
        from: Option<String>,
        to: crate::core::text::TextFontFamily,
    ) -> usize {
        self.ensure_text_font_registered(&to);
        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let to_storage = to.storage_name();
        let matches = |fam: &crate::core::text::TextFontFamily| match &from {
            None => true,
            Some(want) => &fam.storage_name() == want,
        };

        // Collect the layers that actually change (index, anchor origin, new
        // text) before mutating, so one structural command spans the batch.
        let mut edits: Vec<(usize, (i32, i32), TextData)> = Vec::new();
        {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            for (idx, layer) in canvas.layer_stack.layers.iter().enumerate() {
                if layer.locked && !layer.is_background {
                    continue;
                }
                let LayerType::Text(td) = &layer.layer_type else {
                    continue;
                };
                let mut new_td = td.clone();
                let mut changed = false;
                if matches(&new_td.font_family) && new_td.font_family.storage_name() != to_storage {
                    new_td.font_family = to.clone();
                    changed = true;
                }
                for g in &mut new_td.glyph_styles {
                    if matches(&g.font_family) && g.font_family.storage_name() != to_storage {
                        g.font_family = to.clone();
                        changed = true;
                    }
                }
                if changed {
                    // Keep each block centred on its old position (frame-safe)
                    // instead of pinning a corner, which visibly drifts when the
                    // new font's metrics differ. Fall back to the plain anchor
                    // only when there is no ink to measure.
                    let origin = compute_refont_origin(layer, &new_td)
                        .unwrap_or_else(|| text_edit_origin_for_layer(td, layer));
                    edits.push((idx, origin, new_td));
                }
            }
        }

        if edits.is_empty() {
            return 0;
        }

        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let before =
            LayerStructureCommand::capture_before("Change Font", &canvas.layer_stack, cw, ch);
        let count = edits.len();
        for (idx, origin, new_td) in edits {
            rasterize_into_layer(canvas, idx, origin, new_td);
        }
        canvas.layer_revision += 1;
        let mut cmd = before;
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
        count
    }

    /// Build a `TextData` from the current app-level text style + the given
    /// content and per-glyph overrides (compacted when uniform).
    fn text_data_from_state(
        &self,
        content: String,
        glyph_styles: Vec<crate::core::text::GlyphStyle>,
    ) -> TextData {
        let mut td = TextData {
            content,
            font_px: self.edit.text_font_px,
            color: self.edit.text_color,
            font_family: self.edit.text_font_family.clone(),
            align: self.edit.text_align,
            line_height: self.edit.text_line_height,
            bold: self.edit.text_bold,
            italic: self.edit.text_italic,
            underline: self.edit.text_underline,
            tracking_px: self.edit.text_tracking_px,
            opacity: self.edit.text_opacity,
            stretch_x: 1.0,
            rotation_deg: 0.0,
            flip_x: false,
            flip_y: false,
            glyph_styles,
        };
        // Keep glyph_styles either empty (uniform) or exactly one per char.
        if !td.glyph_styles.is_empty() {
            td.ensure_glyph_styles();
            td.compact_glyph_styles();
        }
        td
    }

    /// Hit-test editable text layers from top to bottom at a canvas-space point.
    pub fn text_layer_at(&self, cx: f32, cy: f32) -> Option<usize> {
        const TEXT_HIT_PAD: i32 = 8;

        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let x = cx.floor() as i32;
        let y = cy.floor() as i32;
        for (idx, layer) in canvas.layer_stack.layers.iter().enumerate().rev() {
            if !layer.visible || !matches!(layer.layer_type, LayerType::Text(_)) {
                continue;
            }
            let lx = x - layer.offset.0;
            let ly = y - layer.offset.1;

            // content_bounds is pixel-accurate, so the padded bbox test is the
            // whole hit-test (any opaque pixel lies inside the bbox).
            if let Some((min_x, min_y, max_x, max_y)) = layer.tiles.content_bounds() {
                if lx >= min_x - TEXT_HIT_PAD
                    && lx < max_x + TEXT_HIT_PAD
                    && ly >= min_y - TEXT_HIT_PAD
                    && ly < max_y + TEXT_HIT_PAD
                {
                    return Some(idx);
                }
            }
        }
        None
    }

    /// Click with the Type tool on the canvas. Under the strict modal lock an
    /// open session refuses the click (bell) — it ends only via Commit/Cancel
    /// (✓/✗, Esc, numpad Enter), so clicking away can't spawn or drop text.
    pub fn text_tool_click(&mut self, cx: f32, cy: f32) {
        if self.edit.text_edit.is_some() {
            self.deny_modal_action();
            return;
        }
        self.begin_new_text(cx, cy);
    }

    fn begin_new_text(&mut self, cx: f32, cy: f32) {
        self.edit.text_font_preview = None;
        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let doc_id = self.docs.documents[self.docs.active_doc_idx].id;
        let origin = (cx.round() as i32, cy.round() as i32);
        if self.edit.text_font_px_auto {
            // Until the user picks a size, adapt the default to the canvas so
            // the first click yields readable text on any document size.
            self.edit.text_font_px = auto_font_px(cw, ch);
        }

        let td = self.text_data_from_state(String::new(), Vec::new());
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let before = LayerStructureCommand::capture_before("Type", &canvas.layer_stack, cw, ch);
        let idx = canvas.layer_stack.add_layer(cw, ch);
        let layer = &mut canvas.layer_stack.layers[idx];
        layer.name = "Text".to_string();
        layer.offset = origin;
        let layer_id = layer.id;
        layer.layer_type = LayerType::Text(td);
        canvas.layer_revision += 1;

        self.edit.text_edit = Some(TextEditState {
            doc_id,
            layer_id,
            buffer: String::new(),
            origin,
            rotation_deg: 0.0,
            stretch_x: 1.0,
            flip_x: false,
            flip_y: false,
            is_new: true,
            orig: None,
            before_cmd: Some(before),
            glyph_styles: Vec::new(),
            selection: None,
            caret: Some(0),
            pending_style: None,
        });
        self.edit.text_focus_pending = true;

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Re-open an existing text layer for editing (double-click).
    pub fn begin_edit_text_layer(&mut self, idx: usize) {
        self.edit.text_font_preview = None;
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        let (cw, ch) = {
            let d = &self.docs.documents[self.docs.active_doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let doc_id = self.docs.documents[self.docs.active_doc_idx].id;

        let (td, layer_id, origin) = {
            let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
            if idx >= canvas.layer_stack.layers.len() {
                return;
            }
            let layer = &canvas.layer_stack.layers[idx];
            let LayerType::Text(td) = layer.layer_type.clone() else {
                return;
            };
            let layer_id = layer.id;
            let origin = text_edit_origin_for_layer(&td, layer);
            (td, layer_id, origin)
        };

        self.edit.text_font_px = td.font_px;
        // Re-editing adopts the layer's size as the working default.
        self.edit.text_font_px_auto = false;
        self.ensure_text_font_registered(&td.font_family);
        self.edit.text_font_family = td.font_family.clone();
        self.edit.text_align = td.align;
        self.edit.text_color = td.color;
        self.edit.text_bold = td.bold;
        self.edit.text_italic = td.italic;
        self.edit.text_underline = td.underline;
        self.edit.text_line_height = td.line_height;
        self.edit.text_tracking_px = td.tracking_px;
        self.edit.text_opacity = td.opacity;
        self.edit.fg_color = td.color;

        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let before =
            LayerStructureCommand::capture_before("Edit Text", &canvas.layer_stack, cw, ch);
        // Blank placeholder while the overlay shows the live preview; keep
        // width/height in sync with the tiles (commit/cancel restore both).
        canvas.layer_stack.layers[idx].tiles = TileMap::new(cw, ch);
        canvas.layer_stack.layers[idx].width = cw;
        canvas.layer_stack.layers[idx].height = ch;
        canvas.layer_stack.active_idx = idx;
        canvas.layer_revision += 1;

        // Seed the edit buffer as canonical NFC so the on-screen caret lines up
        // with the fused glyphs and a later commit stays precomposed (see
        // `update_text_buffer`). Legacy text saved as decomposed diacritics is
        // repaired the moment it is re-opened.
        let (nfc_chars, style_src) = crate::core::text::normalize_nfc_with_style_src(&td.content);
        let buffer: String = nfc_chars.iter().collect();
        let glyph_styles = if td.glyph_styles.is_empty() {
            Vec::new()
        } else {
            style_src
                .iter()
                .map(|&i| td.glyph_style(i))
                .collect::<Vec<_>>()
        };
        let caret = Some(nfc_chars.len());

        self.edit.tools.select(ToolId::Text);
        self.shell.ui.show_text_panel = true;
        self.edit.text_edit = Some(TextEditState {
            doc_id,
            layer_id,
            buffer,
            origin,
            rotation_deg: td.rotation_deg,
            stretch_x: td.stretch_x,
            flip_x: td.flip_x,
            flip_y: td.flip_y,
            is_new: false,
            glyph_styles,
            selection: None,
            caret,
            pending_style: None,
            orig: Some(td),
            before_cmd: Some(before),
        });
        self.edit.text_focus_pending = true;

        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Live buffer update from the editing overlay (no rasterization yet — the
    /// overlay shows the text until commit). Keeps per-character styles aligned
    /// with the edited string.
    pub fn update_text_buffer(&mut self, new: String) {
        // Store canonical (precomposed) text: decomposed diacritics would render
        // with detached accents (ab_glyph has no mark positioning) and the caret
        // would count the invisible combining marks. NFC also matches how most
        // Vietnamese IMEs deliver text.
        let new = crate::core::text::normalize_nfc(&new);
        let base = crate::core::text::GlyphStyle {
            color: self.edit.text_color,
            font_px: self.edit.text_font_px,
            font_family: self.edit.text_font_family.clone(),
            bold: self.edit.text_bold,
            italic: self.edit.text_italic,
            underline: self.edit.text_underline,
        };
        let Some(s) = &mut self.edit.text_edit else {
            return;
        };
        if s.buffer == new {
            return;
        }
        let old = std::mem::replace(&mut s.buffer, new);
        resync_glyph_styles(s, &old, base);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Current selection (sorted char range) in the editing overlay, if any.
    fn text_overlay_selection(&self) -> Option<std::ops::Range<usize>> {
        let te_id = egui::Id::new("text_overlay_te");
        let st = egui::TextEdit::load_state(&self.win.egui_ctx, te_id)?;
        Some(st.cursor.char_range()?.as_sorted_char_range())
    }

    /// Current caret char index in the editing overlay (primary cursor).
    fn text_overlay_caret(&self) -> Option<usize> {
        let te_id = egui::Id::new("text_overlay_te");
        let st = egui::TextEdit::load_state(&self.win.egui_ctx, te_id)?;
        Some(st.cursor.char_range()?.primary.index)
    }

    pub fn update_text_cursor_cache(
        &mut self,
        selection: Option<std::ops::Range<usize>>,
        caret: Option<usize>,
    ) {
        let Some(s) = &mut self.edit.text_edit else {
            return;
        };
        let n = s.buffer.chars().count();
        s.selection = selection.and_then(|range| {
            let start = range.start.min(n);
            let end = range.end.min(n);
            (start < end).then_some(start..end)
        });
        s.caret = caret.map(|idx| idx.min(n)).or(s.caret).or(Some(n));
    }

    /// Resolve the case-switch target while editing: the selected char range
    /// (else the whole buffer), the buffer's chars, and whether a real selection
    /// was in play. `None` when there's nothing editable to recase.
    fn text_case_target(&self) -> Option<(Vec<char>, std::ops::Range<usize>, bool)> {
        let selection = self
            .edit
            .text_edit
            .as_ref()
            .and_then(|s| s.selection.clone())
            .or_else(|| {
                self.text_overlay_selection()
                    .filter(|range| range.start < range.end)
            });
        let session = self.edit.text_edit.as_ref()?;
        let chars: Vec<char> = session.buffer.chars().collect();
        let n = chars.len();
        if n == 0 {
            return None;
        }
        let range = match &selection {
            Some(r) => r.start.min(n)..r.end.min(n),
            None => 0..n,
        };
        if range.start >= range.end {
            return None;
        }
        Some((chars, range, selection.is_some()))
    }

    /// Recase `chars[range]` to `target` and write it back into the live edit
    /// session. Per-glyph styles stay aligned (re-spread across any case
    /// expansion like ß→SS), the selection is preserved so repeated presses keep
    /// working, and the change lands in the buffer (committed with the rest of
    /// the edit; no separate undo step).
    fn apply_text_case(
        &mut self,
        chars: Vec<char>,
        range: std::ops::Range<usize>,
        had_selection: bool,
        target: crate::core::text::TextCase,
    ) {
        let n = chars.len();
        let (new_chars, style_src) =
            crate::core::text::transform_text_case(&chars, range.clone(), target);
        if new_chars == chars {
            // Uncased text (digits/punctuation), or already that case.
            return;
        }
        // New selection end after any expansion; the tail length is unchanged.
        let new_end = new_chars.len().saturating_sub(n - range.end);
        let new_selection = had_selection.then(|| range.start..new_end);

        let Some(session) = self.edit.text_edit.as_mut() else {
            return;
        };
        // Re-spread aligned per-glyph styles across the (possibly expanded) span.
        let old_styles = std::mem::take(&mut session.glyph_styles);
        if old_styles.len() == n {
            session.glyph_styles = style_src.iter().map(|&i| old_styles[i].clone()).collect();
        }
        session.buffer = new_chars.into_iter().collect();
        session.selection = new_selection.clone();
        session.caret = Some(new_end);
        // The pending style is pinned to a caret index in the old buffer.
        session.pending_style = None;

        // Push the new caret/selection into egui's invisible TextEdit so the
        // visible selection survives and further presses keep working.
        let te_id = egui::Id::new("text_overlay_te");
        if let Some(mut state) = egui::TextEdit::load_state(&self.win.egui_ctx, te_id) {
            use egui::text::{CCursor, CCursorRange};
            let cursor_range = match &new_selection {
                Some(r) => CCursorRange::two(CCursor::new(r.start), CCursor::new(r.end)),
                None => CCursorRange::one(CCursor::new(new_end)),
            };
            state.cursor.set_char_range(Some(cursor_range));
            state.store(&self.win.egui_ctx, te_id);
        }

        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Shift+F3 while editing: cycle the case of the selected text — or the
    /// whole buffer when nothing is selected — through lowercase → UPPERCASE →
    /// Title Case.
    pub fn text_cycle_case(&mut self) {
        let Some((chars, range, had_selection)) = self.text_case_target() else {
            return;
        };
        let span: String = chars[range.clone()].iter().collect();
        let target = crate::core::text::next_text_case(&span);
        self.apply_text_case(chars, range, had_selection, target);
    }

    /// Set an explicit case (lowercase / UPPERCASE / Title Case) on the selected
    /// text — or the whole buffer — while editing. Drives the Text panel's case
    /// buttons (the discoverable form of the Shift+F3 shortcut).
    pub fn text_set_case(&mut self, target: crate::core::text::TextCase) {
        let Some((chars, range, had_selection)) = self.text_case_target() else {
            return;
        };
        self.apply_text_case(chars, range, had_selection, target);
    }

    /// Apply a colour and/or size override to the current text selection while
    /// editing. With no selection it becomes a pending style for the next typed
    /// character. Must be called BEFORE updating the app-level default so the
    /// materialization seed reflects the unselected characters' prior style.
    pub fn apply_text_style_to_selection(
        &mut self,
        color: Option<[u8; 4]>,
        font_px: Option<f32>,
        font_family: Option<crate::core::text::TextFontFamily>,
        bold: Option<bool>,
        italic: Option<bool>,
        underline: Option<bool>,
    ) {
        let base = crate::core::text::GlyphStyle {
            color: self.edit.text_color,
            font_px: self.edit.text_font_px,
            font_family: self.edit.text_font_family.clone(),
            bold: self.edit.text_bold,
            italic: self.edit.text_italic,
            underline: self.edit.text_underline,
        };
        let selection = self
            .edit
            .text_edit
            .as_ref()
            .and_then(|s| s.selection.as_ref().cloned())
            .or_else(|| {
                self.text_overlay_selection()
                    .filter(|range| range.start < range.end)
            });
        let caret = self
            .edit
            .text_edit
            .as_ref()
            .and_then(|s| s.caret)
            .or_else(|| self.text_overlay_caret());
        let Some(s) = &mut self.edit.text_edit else {
            return;
        };
        let n = s.buffer.chars().count();
        let apply = |style: &mut crate::core::text::GlyphStyle| {
            if let Some(c) = color {
                style.color = c;
            }
            if let Some(px) = font_px {
                style.font_px = px.clamp(4.0, 1600.0);
            }
            if let Some(family) = &font_family {
                style.font_family = family.clone();
            }
            if let Some(v) = bold {
                style.bold = v;
            }
            if let Some(v) = italic {
                style.italic = v;
            }
            if let Some(v) = underline {
                style.underline = v;
            }
        };
        match selection {
            Some(range) if range.start < range.end => {
                if s.glyph_styles.len() != n {
                    s.glyph_styles = (0..n)
                        .map(|i| s.glyph_styles.get(i).cloned().unwrap_or(base.clone()))
                        .collect();
                }
                for i in range.start..range.end.min(n) {
                    if let Some(style) = s.glyph_styles.get_mut(i) {
                        apply(style);
                    }
                }
                s.pending_style = None;
            }
            _ => {
                let caret = caret.unwrap_or(n).min(n);
                let mut style = s.pending_style.clone().map(|(_, st)| st).unwrap_or(base);
                apply(&mut style);
                s.pending_style = Some((caret, style));
            }
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    fn apply_text_font_family_choice(&mut self, family: crate::core::text::TextFontFamily) {
        self.ensure_text_font_registered(&family);
        if self.edit.text_edit.is_some() {
            self.apply_text_style_to_selection(None, None, Some(family.clone()), None, None, None);
        }
        self.edit.text_font_family = family;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    pub fn preview_text_font_family(&mut self, family: crate::core::text::TextFontFamily) {
        if self.edit.text_font_family == family {
            return;
        }
        if self.edit.text_font_preview.is_none() {
            self.edit.text_font_preview = Some(TextFontPreviewState {
                font_family: self.edit.text_font_family.clone(),
                session: self
                    .edit
                    .text_edit
                    .as_ref()
                    .map(|session| TextFontPreviewSession {
                        glyph_styles: session.glyph_styles.clone(),
                        pending_style: session.pending_style.clone(),
                        selection: session.selection.clone(),
                        caret: session.caret,
                    }),
            });
        }
        self.apply_text_font_family_choice(family);
    }

    pub fn commit_text_font_family(&mut self, family: crate::core::text::TextFontFamily) {
        self.edit.text_font_preview = None;
        self.apply_text_font_family_choice(family);
    }

    pub fn cancel_text_font_family_preview(&mut self) {
        let Some(snapshot) = self.edit.text_font_preview.take() else {
            return;
        };
        let family = snapshot.font_family;
        self.ensure_text_font_registered(&family);
        self.edit.text_font_family = family;
        if let (Some(session), Some(session_snapshot)) =
            (self.edit.text_edit.as_mut(), snapshot.session)
        {
            let n = session.buffer.chars().count();
            if session_snapshot.glyph_styles.is_empty() || session_snapshot.glyph_styles.len() == n
            {
                session.glyph_styles = session_snapshot.glyph_styles;
            }
            session.pending_style = session_snapshot.pending_style;
            session.selection = session_snapshot.selection;
            session.caret = session_snapshot.caret;
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Finalize the current text session: rasterize the buffer into the layer
    /// and push an undo entry.
    ///
    /// Resolves the session's document by id — the session may outlive a tab
    /// switch, and dropping it against the wrong document would leave the
    /// layer as the blank editing placeholder (losing the original text).
    pub fn commit_text_edit(&mut self) {
        let Some(session) = self.edit.text_edit.take() else {
            return;
        };
        self.edit.text_font_preview = None;
        self.edit.text_focus_pending = false;
        let Some(doc_idx) = self
            .docs
            .documents
            .iter()
            .position(|d| d.id == session.doc_id)
        else {
            return;
        };
        let is_active = doc_idx == self.docs.active_doc_idx;
        let (cw, ch) = {
            let d = &self.docs.documents[doc_idx];
            (d.canvas.width, d.canvas.height)
        };
        let mut td =
            self.text_data_from_state(session.buffer.clone(), session.glyph_styles.clone());
        td.rotation_deg = session.rotation_deg;
        td.stretch_x = session.stretch_x;
        td.flip_x = session.flip_x;
        td.flip_y = session.flip_y;

        let canvas = &mut self.docs.documents[doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == session.layer_id)
        else {
            return;
        };

        let empty = session.buffer.trim().is_empty();

        if empty {
            if session.is_new {
                canvas.layer_stack.remove_layer(idx);
            } else if let Some(orig) = session.orig {
                rasterize_into_layer(canvas, idx, session.origin, orig);
            }
            canvas.layer_revision += 1;
            if is_active {
                self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
            }
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
            return;
        }

        let changed = session.is_new || session.orig.as_ref() != Some(&td);
        rasterize_into_layer(canvas, idx, session.origin, td);

        if changed {
            if let Some(mut cmd) = session.before_cmd {
                cmd.capture_after(&canvas.layer_stack, cw, ch);
                canvas.record(Box::new(cmd));
            }
        }
        canvas.layer_revision += 1;

        if is_active {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if changed {
            if is_active {}
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Install `family` into the egui context if it isn't yet. Only builtin
    /// families load at startup; anything else (System fonts, fonts referenced
    /// by opened documents) registers here the first time it is used. The
    /// canvas preview rasterizes from the font file directly, so this only
    /// affects the invisible editing TextEdit (click→caret mapping).
    pub fn ensure_text_font_registered(&mut self, family: &crate::core::text::TextFontFamily) {
        let id = family.egui_family_name();
        if self.edit.text_fonts_registered.contains(&id)
            || self.edit.text_fonts_failed.contains(&id)
        {
            return;
        }
        // Startup fonts not installed yet: builtins arrive with FontsReady,
        // and the family setters re-run this once the app is interactive.
        let Some(fonts) = self.win.ui_fonts.as_mut() else {
            return;
        };
        let Some(data) = crate::core::text::validated_font_bytes_for(family) else {
            self.edit.text_fonts_failed.insert(id);
            return;
        };
        fonts.font_data.insert(
            id.clone(),
            std::sync::Arc::new(egui::FontData::from_owned(data)),
        );
        fonts
            .families
            .insert(egui::FontFamily::Name(id.clone().into()), vec![id.clone()]);
        self.win.egui_ctx.set_fonts(fonts.clone());
        self.edit.text_fonts_registered.insert(id);
    }

    /// Abort the current text session, discarding changes. Like commit, the
    /// session's document is resolved by id so a stale session still restores
    /// the original layer content.
    pub fn cancel_text_edit(&mut self) {
        let Some(session) = self.edit.text_edit.take() else {
            return;
        };
        self.edit.text_font_preview = None;
        self.edit.text_focus_pending = false;
        let Some(doc_idx) = self
            .docs
            .documents
            .iter()
            .position(|d| d.id == session.doc_id)
        else {
            return;
        };
        let is_active = doc_idx == self.docs.active_doc_idx;
        let canvas = &mut self.docs.documents[doc_idx].canvas;
        if let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == session.layer_id)
        {
            if session.is_new {
                canvas.layer_stack.remove_layer(idx);
            } else if let Some(orig) = session.orig {
                rasterize_into_layer(canvas, idx, session.origin, orig);
            }
            canvas.layer_revision += 1;
        }
        if is_active {
            self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_text(content: &str) -> (App, usize) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let canvas = &mut app.docs.documents[0].canvas;
        let idx = canvas.layer_stack.add_layer(200, 200);
        let td = TextData {
            content: content.to_string(),
            font_family: crate::core::text::TextFontFamily::DejaVuSans,
            font_px: 48.0,
            ..TextData::default()
        };
        rasterize_into_layer(canvas, idx, (20, 30), td);
        canvas.layer_stack.active_idx = idx;
        (app, idx)
    }

    #[test]
    fn text_to_curves_preserves_mask_and_undo_redo_selection() {
        let (mut app, idx) = app_with_text("Aa");
        {
            let layer = &mut app.docs.documents[0].canvas.layer_stack.layers[idx];
            layer.add_mask(false);
            layer.selected = false;
        }

        assert!(app.text_to_curves(idx));
        let canvas = &mut app.docs.documents[0].canvas;
        assert!(
            canvas.layer_stack.layers[idx].mask.is_some(),
            "conversion must preserve the text layer mask"
        );
        assert!(canvas.layer_stack.layers[idx].selected);
        assert!(matches!(
            canvas.layer_stack.layers[idx].layer_type,
            LayerType::Vector(crate::core::vector::object::VectorGeometry::Path(_))
        ));

        canvas.undo().expect("undo text-to-curves");
        assert!(matches!(
            canvas.layer_stack.layers[idx].layer_type,
            LayerType::Text(_)
        ));
        assert!(
            !canvas.layer_stack.layers[idx].selected,
            "undo restores the original selection"
        );

        canvas.redo().expect("redo text-to-curves");
        assert!(canvas.layer_stack.layers[idx].mask.is_some());
        assert!(
            canvas.layer_stack.layers[idx].selected,
            "redo restores the conversion's selection"
        );
    }

    #[test]
    fn click_outside_is_refused_while_editing() {
        let mut app = App::new();
        app.text_tool_click(10.0, 10.0);
        assert!(app.edit.text_edit.is_some());
        let layers_during = app.docs.documents[0].canvas.layer_stack.layers.len();

        app.update_text_buffer("Hi".to_string());
        app.text_tool_click(200.0, 150.0);

        assert!(
            app.edit.text_edit.is_some(),
            "the session stays open until an explicit commit/cancel"
        );
        assert_eq!(
            app.docs.documents[0].canvas.layer_stack.layers.len(),
            layers_during,
            "a refused click must not spawn another text layer"
        );

        app.commit_text_edit();
        assert!(
            app.edit.text_edit.is_none(),
            "explicit commit ends the session"
        );
    }

    #[test]
    fn shift_f3_cycles_case_of_whole_buffer() {
        let mut app = App::new();
        app.text_tool_click(5.0, 5.0);
        app.update_text_buffer("hello world".to_string());
        // No selection → operate on the whole buffer.
        app.update_text_cursor_cache(None, Some(11));

        app.text_cycle_case();
        assert_eq!(app.edit.text_edit.as_ref().unwrap().buffer, "HELLO WORLD");
        app.text_cycle_case();
        assert_eq!(app.edit.text_edit.as_ref().unwrap().buffer, "Hello World");
        app.text_cycle_case();
        assert_eq!(app.edit.text_edit.as_ref().unwrap().buffer, "hello world");
    }

    #[test]
    fn text_panel_case_buttons_set_explicit_case() {
        let mut app = App::new();
        app.text_tool_click(5.0, 5.0);
        app.update_text_buffer("hello world".to_string());
        app.update_text_cursor_cache(None, Some(11));

        app.text_set_case(crate::core::text::TextCase::Upper);
        assert_eq!(app.edit.text_edit.as_ref().unwrap().buffer, "HELLO WORLD");
        // Idempotent: the same button again is a no-op, not a cycle.
        app.text_set_case(crate::core::text::TextCase::Upper);
        assert_eq!(app.edit.text_edit.as_ref().unwrap().buffer, "HELLO WORLD");
        app.text_set_case(crate::core::text::TextCase::Title);
        assert_eq!(app.edit.text_edit.as_ref().unwrap().buffer, "Hello World");
        app.text_set_case(crate::core::text::TextCase::Lower);
        assert_eq!(app.edit.text_edit.as_ref().unwrap().buffer, "hello world");
    }

    #[test]
    fn shift_f3_recases_only_the_selection_and_keeps_glyph_styles_aligned() {
        let mut app = App::new();
        app.text_tool_click(5.0, 5.0);
        app.update_text_buffer("abcd".to_string());
        // Give char 1 a distinct colour so we can prove styles stay aligned.
        app.update_text_cursor_cache(Some(1..2), Some(2));
        app.apply_text_style_to_selection(Some([200, 30, 30, 255]), None, None, None, None, None);

        // Select "bc" and cycle case → "BC".
        app.update_text_cursor_cache(Some(1..3), Some(3));
        app.text_cycle_case();

        let s = app.edit.text_edit.as_ref().unwrap();
        assert_eq!(s.buffer, "aBCd", "only the selected span is recased");
        assert_eq!(s.selection, Some(1..3), "the selection is preserved");
        assert_eq!(s.glyph_styles.len(), 4);
        assert_eq!(
            s.glyph_styles[1].color,
            [200, 30, 30, 255],
            "per-glyph colour follows its character through the recase"
        );
    }

    #[test]
    fn recolor_text_layer_recolours_type_and_is_undoable() {
        let (mut app, idx) = app_with_text("Hi");
        let orig = match &app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type {
            LayerType::Text(td) => td.color,
            _ => panic!("expected a text layer"),
        };
        let new_color = [10, 120, 200, 255];
        assert_ne!(orig, new_color);

        assert!(app.recolor_text_layer(idx, new_color));
        match &app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type {
            LayerType::Text(td) => assert_eq!(td.color, new_color),
            _ => panic!("still a text layer"),
        }
        // Re-applying the same colour is a no-op (nothing to record).
        assert!(!app.recolor_text_layer(idx, new_color));

        app.docs.documents[0]
            .canvas
            .undo()
            .expect("undo the recolour");
        match &app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type {
            LayerType::Text(td) => assert_eq!(td.color, orig, "undo restores the original colour"),
            _ => panic!("undo keeps it a text layer"),
        }
    }

    /// Add a Text layer with the given font to doc 0, returning its index.
    fn add_text_layer(
        app: &mut App,
        content: &str,
        font: crate::core::text::TextFontFamily,
    ) -> usize {
        let canvas = &mut app.docs.documents[0].canvas;
        let (cw, ch) = (canvas.width, canvas.height);
        let idx = canvas.layer_stack.add_layer(cw, ch);
        let td = TextData {
            content: content.to_string(),
            font_family: font,
            font_px: 48.0,
            ..TextData::default()
        };
        rasterize_into_layer(canvas, idx, (10, 10), td);
        idx
    }

    fn layer_font(app: &App, idx: usize) -> crate::core::text::TextFontFamily {
        match &app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type {
            LayerType::Text(td) => td.font_family.clone(),
            _ => panic!("expected a text layer"),
        }
    }

    #[test]
    fn refont_origin_centres_new_block_on_old_centre() {
        // Old ink centre (canvas) = (100+25, 100+10) = (125, 110).
        let origin = refont_origin_from_bounds((100, 100), (0, 0, 50, 20), (0, 0, 80, 30), (0, 0));
        assert_eq!(origin, (85, 95));
        // Placing the new block (ink centre local (40, 15)) at that origin lands
        // its centre exactly on the old centre.
        assert_eq!((origin.0 + 40, origin.1 + 15), (125, 110));
    }

    #[test]
    fn refont_origin_accounts_for_placed_delta() {
        // Non-zero placed delta (rotated/stretched): origin subtracts it so
        // layer.offset (= origin + delta) still centres the block. Same bounds →
        // centres equal → final offset 0 → origin = -delta.
        let delta = (7, -3);
        let origin = refont_origin_from_bounds((0, 0), (0, 0, 40, 40), (0, 0, 40, 40), delta);
        assert_eq!(origin, (-7, 3));
        assert_eq!((origin.0 + delta.0, origin.1 + delta.1), (0, 0));
    }

    #[test]
    fn change_all_text_font_switches_every_layer_and_is_one_undo() {
        use crate::core::text::TextFontFamily;
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let a = add_text_layer(&mut app, "Alpha", TextFontFamily::DejaVuSans);
        let b = add_text_layer(&mut app, "Beta", TextFontFamily::Arial);

        // from = None → every text layer becomes LiberationSans.
        let n = app.change_document_text_font(None, TextFontFamily::LiberationSans);
        assert_eq!(n, 2, "both text layers change");
        assert_eq!(layer_font(&app, a), TextFontFamily::LiberationSans);
        assert_eq!(layer_font(&app, b), TextFontFamily::LiberationSans);

        // A single undo restores BOTH original fonts.
        app.docs.documents[0]
            .canvas
            .undo()
            .expect("undo batch font");
        assert_eq!(layer_font(&app, a), TextFontFamily::DejaVuSans);
        assert_eq!(layer_font(&app, b), TextFontFamily::Arial);
    }

    #[test]
    fn replace_specific_font_leaves_other_fonts_untouched() {
        use crate::core::text::TextFontFamily;
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let a = add_text_layer(&mut app, "Alpha", TextFontFamily::DejaVuSans);
        let b = add_text_layer(&mut app, "Beta", TextFontFamily::Arial);

        // Replace only DejaVuSans → Tahoma; Arial layer must stay Arial.
        let n = app.change_document_text_font(
            Some(TextFontFamily::DejaVuSans.storage_name()),
            TextFontFamily::Tahoma,
        );
        assert_eq!(n, 1, "only the DejaVuSans layer changes");
        assert_eq!(layer_font(&app, a), TextFontFamily::Tahoma);
        assert_eq!(layer_font(&app, b), TextFontFamily::Arial);
    }

    #[test]
    fn change_font_covers_per_glyph_overrides() {
        use crate::core::text::{GlyphStyle, TextFontFamily};
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let idx = add_text_layer(&mut app, "Hi", TextFontFamily::DejaVuSans);
        // Give the second character a distinct per-glyph font.
        {
            let layer = &mut app.docs.documents[0].canvas.layer_stack.layers[idx];
            if let LayerType::Text(td) = &mut layer.layer_type {
                td.glyph_styles = vec![td.glyph_style(0), td.glyph_style(1)];
                td.glyph_styles[1].font_family = TextFontFamily::Arial;
            }
        }

        let n = app.change_document_text_font(None, TextFontFamily::Calibri);
        assert_eq!(n, 1);
        match &app.docs.documents[0].canvas.layer_stack.layers[idx].layer_type {
            LayerType::Text(td) => {
                assert_eq!(td.font_family, TextFontFamily::Calibri);
                assert!(
                    td.glyph_styles
                        .iter()
                        .all(|g| g.font_family == TextFontFamily::Calibri),
                    "per-glyph fonts switch too"
                );
            }
            _ => panic!("still a text layer"),
        }
    }

    #[test]
    fn fonts_in_use_lists_base_and_glyph_fonts() {
        use crate::core::text::TextFontFamily;
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let idx = add_text_layer(&mut app, "Hi", TextFontFamily::DejaVuSans);
        {
            let layer = &mut app.docs.documents[0].canvas.layer_stack.layers[idx];
            if let LayerType::Text(td) = &mut layer.layer_type {
                td.glyph_styles = vec![td.glyph_style(0), td.glyph_style(1)];
                td.glyph_styles[1].font_family = TextFontFamily::Arial;
            }
        }
        let storages: Vec<String> = app
            .document_text_fonts_in_use()
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        assert!(storages.contains(&TextFontFamily::DejaVuSans.storage_name()));
        assert!(storages.contains(&TextFontFamily::Arial.storage_name()));
    }

    #[test]
    fn text_session_engages_the_modal_lock() {
        let mut app = App::new();
        assert!(!app.modal_lock_active());
        app.text_tool_click(10.0, 10.0);
        assert!(app.modal_lock_active(), "editing text locks other features");
        app.cancel_text_edit();
        assert!(!app.modal_lock_active());
    }

    #[test]
    fn commit_lands_in_the_sessions_document_after_a_doc_switch() {
        let mut app = App::new();
        app.open_new_doc_tab();
        assert_eq!(app.docs.active_doc_idx, 1);
        app.text_tool_click(10.0, 10.0);
        app.update_text_buffer("giữ chữ".to_string());
        // A doc switch that bypasses switch_to_doc's implicit commit
        // (async/AI paths assign active_doc_idx directly).
        app.docs.active_doc_idx = 0;
        app.commit_text_edit();

        assert!(app.edit.text_edit.is_none());
        let doc1 = &app.docs.documents[1];
        assert!(
            doc1.canvas.layer_stack.layers.iter().any(|l| matches!(
                &l.layer_type,
                LayerType::Text(td) if td.content == "giữ chữ"
            )),
            "text must be finalized into its own document"
        );
        assert!(doc1.is_modified(), "the owning document carries the change");
    }

    #[test]
    fn pending_style_survives_backspace_then_applies_on_type() {
        let mut app = App::new();
        app.text_tool_click(5.0, 5.0);
        app.update_text_buffer("ab".to_string());
        // Colour picked with the caret at the end and no selection → pending.
        app.update_text_cursor_cache(None, Some(2));
        app.apply_text_style_to_selection(Some([200, 30, 30, 255]), None, None, None, None, None);

        app.update_text_buffer("a".to_string());
        app.update_text_cursor_cache(None, Some(1));
        app.update_text_buffer("ax".to_string());

        let s = app.edit.text_edit.as_ref().expect("session still open");
        assert_eq!(s.glyph_styles.len(), 2);
        assert_eq!(
            s.glyph_styles[1].color,
            [200, 30, 30, 255],
            "pending colour must survive the Backspace and land on the typed char"
        );
    }

    #[test]
    fn selected_bold_applies_only_to_selected_chars() {
        let mut app = App::new();
        app.text_tool_click(5.0, 5.0);
        app.update_text_buffer("abcd".to_string());
        app.update_text_cursor_cache(Some(1..3), Some(3));

        app.apply_text_style_to_selection(None, None, None, Some(true), None, None);

        let s = app.edit.text_edit.as_ref().expect("session still open");
        assert_eq!(s.glyph_styles.len(), 4);
        assert!(!s.glyph_styles[0].bold);
        assert!(s.glyph_styles[1].bold);
        assert!(s.glyph_styles[2].bold);
        assert!(!s.glyph_styles[3].bold);
    }

    #[test]
    fn font_hover_preview_cancel_restores_mixed_glyph_styles() {
        let mut app = App::new();
        app.text_tool_click(5.0, 5.0);
        app.update_text_buffer("abcd".to_string());
        app.update_text_cursor_cache(Some(1..3), Some(3));

        let original_family = app.edit.text_font_family.clone();
        let original_styles: Vec<_> = "abcd"
            .chars()
            .enumerate()
            .map(|(i, _)| crate::core::text::GlyphStyle {
                color: [0, 0, 0, 255],
                font_px: 48.0,
                font_family: if i % 2 == 0 {
                    crate::core::text::TextFontFamily::SegoeUi
                } else {
                    crate::core::text::TextFontFamily::Tahoma
                },
                bold: i == 0,
                italic: i == 1,
                underline: i == 2,
            })
            .collect();
        app.edit.text_edit.as_mut().unwrap().glyph_styles = original_styles.clone();

        app.preview_text_font_family(crate::core::text::TextFontFamily::Arial);
        assert!(
            app.edit.text_edit.as_ref().unwrap().glyph_styles[1].font_family
                == crate::core::text::TextFontFamily::Arial
        );

        app.cancel_text_font_family_preview();

        let s = app.edit.text_edit.as_ref().expect("session still open");
        assert_eq!(app.edit.text_font_family, original_family);
        assert_eq!(s.glyph_styles, original_styles);
    }
}
