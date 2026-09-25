//! Refine Selection panel: the canvas preview, live slider updates, in-panel
//! undo, and the commit / output modes. The edge engine and the session state
//! live in `core::refine` (on the document's canvas).

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::refine::{Rect, RefineParams};
use crate::ui::refine_select::{RefineOutputMode, RefineViewMode};

/// Largest side of the preview texture.
const MAX_OVERLAY_TEX: usize = 4096;
/// A full re-render faster than this follows a slider live; slower ones wait
/// for the slider to be released.
const LIVE_RENDER_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

impl App {
    fn refine_canvas(&mut self) -> &mut crate::core::canvas::Canvas {
        &mut self.docs.documents[self.docs.active_doc_idx].canvas
    }

    /// Marching ants are hidden while the panel previews the selection
    /// another way.
    pub(in crate::app) fn refine_hides_ants(&self) -> bool {
        self.edit.show_refine_panel
            && (self.edit.refine_show_original
                || self.edit.refine_view_mode != RefineViewMode::MarchingAnts)
    }

    /// Colour of each mask value in the preview, or None when the view draws
    /// no preview (Marching Ants, or Show Original).
    fn refine_overlay_lut(&self) -> Option<Vec<egui::Color32>> {
        if self.edit.refine_show_original {
            return None;
        }
        let tint = |rgb: [u8; 3], opacity: f32| -> Vec<egui::Color32> {
            (0..256)
                .map(|m| {
                    let a = (1.0 - m as f32 / 255.0) * opacity;
                    let [r, g, b, a] = super::ui_data::premultiply_for_linear_target(rgb, a);
                    egui::Color32::from_rgba_premultiplied(r, g, b, a)
                })
                .collect()
        };
        match self.edit.refine_view_mode {
            RefineViewMode::Overlay => {
                let [r, g, b, a] = self.edit.refine_overlay_color;
                Some(tint([r, g, b], a as f32 / 255.0))
            }
            RefineViewMode::OnBlack => Some(tint([0, 0, 0], self.edit.refine_view_opacity)),
            RefineViewMode::OnWhite => Some(tint([255, 255, 255], self.edit.refine_view_opacity)),
            RefineViewMode::BlackWhite => Some(
                (0..256)
                    .map(|m| egui::Color32::from_gray(m as u8))
                    .collect(),
            ),
            RefineViewMode::MarchingAnts => None,
        }
    }

    /// Drop the preview texture so the next frame rebuilds it whole (view,
    /// colour or opacity changed).
    pub(in crate::app) fn invalidate_refine_overlay(&mut self) {
        self.edit.refine_overlay_tex = None;
        self.edit.refine_overlay_mask_rev = u64::MAX;
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Keep the preview texture in step with the live selection: whole on the
    /// first frame, then only the area the last render rewrote.
    pub fn update_refine_overlay_tex(&mut self) {
        if !self.edit.show_refine_panel {
            return;
        }
        let lut = self.refine_overlay_lut();
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let dirty = canvas
            .refine
            .as_deref_mut()
            .and_then(|s| s.take_display_dirty());
        let Some(lut) = lut else {
            self.edit.refine_overlay_tex = None;
            return;
        };
        let (cw, ch) = (canvas.width as usize, canvas.height as usize);
        let mask = &canvas.selection.mask;
        if cw == 0 || ch == 0 || mask.len() < cw * ch {
            return;
        }
        let rev = canvas.selection.mask_revision;
        let (tw, th) = (cw.min(MAX_OVERLAY_TEX), ch.min(MAX_OVERLAY_TEX));
        let patchable = self
            .edit
            .refine_overlay_tex
            .as_ref()
            .is_some_and(|t| t.size() == [tw, th]);
        if patchable && rev == self.edit.refine_overlay_mask_rev {
            return;
        }
        let region = match (patchable, dirty) {
            (true, Some(d)) => Rect {
                x0: d.x0 * tw / cw,
                y0: d.y0 * th / ch,
                x1: (d.x1 * tw).div_ceil(cw).min(tw),
                y1: (d.y1 * th).div_ceil(ch).min(th),
            },
            _ => Rect::full(tw, th),
        };
        let (rw, rh) = (region.width(), region.height());
        if rw == 0 || rh == 0 {
            self.edit.refine_overlay_mask_rev = rev;
            return;
        }
        use rayon::prelude::*;
        let mut pixels = vec![egui::Color32::TRANSPARENT; rw * rh];
        pixels.par_chunks_mut(rw).enumerate().for_each(|(ry, row)| {
            let cy = (region.y0 + ry) * ch / th;
            for (rx, px) in row.iter_mut().enumerate() {
                let cx = (region.x0 + rx) * cw / tw;
                *px = lut[mask[cy * cw + cx] as usize];
            }
        });
        let image = egui::ColorImage {
            size: [rw, rh],
            pixels,
            source_size: egui::Vec2::new(rw as f32, rh as f32),
        };
        match &mut self.edit.refine_overlay_tex {
            Some(tex) if patchable => {
                tex.set_partial([region.x0, region.y0], image, egui::TextureOptions::LINEAR)
            }
            _ => {
                self.edit.refine_overlay_tex = Some(self.win.egui_ctx.load_texture(
                    "sam_overlay",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
        self.edit.refine_overlay_mask_rev = rev;
    }

    /// Open the panel on the active document's selection and pick up the
    /// Refine Brush.
    pub fn open_refine_panel(&mut self) {
        if self.docs.documents.is_empty() || self.edit.show_refine_panel {
            return;
        }
        if self.modal_lock_active() {
            self.deny_modal_action();
            return;
        }
        self.refine_canvas().refine_open();
        self.edit.show_refine_panel = true;
        self.edit.refine_decontaminate = false;
        self.edit.refine_decontaminate_amount = 0.5;
        self.edit.refine_output_mode = RefineOutputMode::Selection;
        self.edit.refine_show_original = false;
        self.edit.refine_render_pending = false;
        self.shell.ui.show_refine_color_dialog = false;
        self.shell.ui.refine_color_dialog_center_next = false;
        self.edit.refine_prev_tool = self.edit.tools.active_id();
        self.edit.tools.select(crate::tools::ToolId::RefineBrush);
        self.invalidate_refine_overlay();
        self.apply_canvas_event(CanvasEvent::SelectionChanged);
    }

    pub(in crate::app) fn refine_params(&self) -> RefineParams {
        self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .refine
            .as_ref()
            .map(|s| s.params())
            .unwrap_or_default()
    }

    /// Slider / checkbox change. `release` is set once the control lets go;
    /// until then the preview follows only when a full render is quick.
    pub(in crate::app) fn set_refine_params(
        &mut self,
        params: Option<RefineParams>,
        release: bool,
    ) {
        let canvas = self.refine_canvas();
        let Some(session) = canvas.refine.as_deref_mut() else {
            return;
        };
        let changed = params.is_some_and(|p| session.set_params(p));
        let live = session.last_full_render() <= LIVE_RENDER_BUDGET;
        if changed {
            self.edit.refine_render_pending = true;
        }
        if self.edit.refine_render_pending && (release || live) {
            self.edit.refine_render_pending = false;
            self.refine_canvas().refine_render(None);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
    }

    /// Ctrl+Z / Ctrl+Shift+Z inside the panel: step the Refine Brush strokes.
    pub(in crate::app) fn refine_history_step(&mut self, redo: bool) {
        if self.refine_canvas().refine_step(redo) {
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        } else {
            self.shell.status_msg = if redo {
                "Nothing to redo in Refine Selection".to_string()
            } else {
                "Nothing to undo in Refine Selection".to_string()
            };
        }
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Clear / Invert the mask being refined (one undo step in the panel).
    pub(in crate::app) fn refine_edit_base(&mut self, invert: bool) {
        let canvas = self.refine_canvas();
        let Some(session) = canvas.refine.as_deref_mut() else {
            return;
        };
        let changed = session.edit_base(|m| {
            if invert {
                m.iter_mut().for_each(|v| *v = 255 - *v);
            } else {
                m.fill(0);
            }
        });
        if changed {
            self.refine_canvas().refine_render(None);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
    }

    fn close_refine_ui(&mut self) {
        self.edit.show_refine_panel = false;
        self.edit.refine_render_pending = false;
        self.edit.refine_show_original = false;
        self.shell.ui.show_refine_color_dialog = false;
        self.shell.ui.refine_color_dialog_center_next = false;
        self.edit.refine_output_mode = RefineOutputMode::Selection;
        self.edit.refine_overlay_tex = None;
        self.edit.refine_overlay_mask_rev = u64::MAX;
        self.edit.tools.active_on_cancel();
        self.edit.tools.select(self.edit.refine_prev_tool);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Cancel: the selection goes back exactly as it was; nothing is recorded.
    pub fn cancel_refine_panel(&mut self) {
        let canvas = self.refine_canvas();
        if let Some(session) = canvas.refine.take() {
            let rev = canvas.selection.mask_revision;
            canvas.selection = session.original;
            canvas.selection.mask_revision = rev + 1;
            canvas.selection.mark_bbox_dirty();
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        self.close_refine_ui();
    }

    /// OK: apply the output mode as ONE undo step ("Refine Selection").
    pub fn commit_refine_panel(&mut self) {
        if self.edit.refine_render_pending {
            self.edit.refine_render_pending = false;
            self.refine_canvas().refine_render(None);
        }
        let Some(session) = self.refine_canvas().refine.take() else {
            self.close_refine_ui();
            return;
        };
        let mut mode = self.edit.refine_output_mode;
        if self.edit.refine_decontaminate
            && matches!(
                mode,
                RefineOutputMode::Selection | RefineOutputMode::LayerMask
            )
        {
            mode = RefineOutputMode::NewLayerWithMask;
        }
        let decontaminate = self
            .edit
            .refine_decontaminate
            .then_some(self.edit.refine_decontaminate_amount);

        let canvas = self.refine_canvas();
        let (cw, ch) = (canvas.width, canvas.height);
        let refined = canvas.selection.clone();
        let mut group = crate::core::command::CompoundCommand::new("Refine Selection");
        if mode != RefineOutputMode::Selection {
            let mut cmd = crate::core::command::LayerStructureCommand::capture_before(
                "Refine Selection",
                &canvas.layer_stack,
                cw,
                ch,
            );
            let done = output_refined_selection(canvas, &refined, mode, decontaminate);
            if done {
                cmd.capture_after(&canvas.layer_stack, cw, ch);
                group.commands.push(Box::new(cmd));
                canvas.layer_revision += 1;
                // Photoshop: the selection is used up by a mask / layer output.
                canvas.selection.deselect();
            } else {
                self.shell.status_msg =
                    "Refine Selection: không xuất được ra layer này — giữ làm vùng chọn"
                        .to_string();
            }
        }
        let canvas = self.refine_canvas();
        let original = &session.original;
        let sel = &canvas.selection;
        if sel.mask != original.mask
            || sel.offset != original.offset
            || sel.active != original.active
        {
            let mut cmd = crate::core::command::SelectionCommand::capture_before(
                "Refine Selection",
                original,
            );
            cmd.capture_after(sel);
            group.commands.push(Box::new(cmd));
        }
        if !group.commands.is_empty() {
            canvas.record(Box::new(group));
        }
        self.close_refine_ui();
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        self.apply_canvas_event(CanvasEvent::SelectionChanged);
    }
}

/// Layer mask (in the layer's own pixel space) revealing `sel`.
fn layer_mask_from_selection(
    sel: &crate::core::selection::Selection,
    layer: &crate::core::layer::Layer,
    cw: u32,
    ch: u32,
) -> Option<crate::core::layer::LayerMask> {
    use rayon::prelude::*;
    let (lw, lh) = (layer.width, layer.height);
    let len = crate::core::canvas::Canvas::guarded_flat_rgba_len(lw, lh)?;
    let (ox, oy) = layer.offset;
    let mut rgba = vec![0u8; len];
    rgba.par_chunks_mut(lw as usize * 4)
        .enumerate()
        .for_each(|(ly, row)| {
            let cy = ly as i64 + oy as i64;
            for (lx, px) in row.chunks_exact_mut(4).enumerate() {
                let cx = lx as i64 + ox as i64;
                let m = if cx >= 0 && cy >= 0 && cx < cw as i64 && cy < ch as i64 {
                    sel.mask[cy as usize * cw as usize + cx as usize]
                } else {
                    0
                };
                px.copy_from_slice(&[m, m, m, 255]);
            }
        });
    Some(crate::core::layer::LayerMask {
        tiles: crate::core::tile::TileMap::from_rgba(&rgba, lw, lh),
        width: lw,
        height: lh,
        enabled: true,
        inverted: false,
        bake_offset: (0, 0),
        bake_frame_offset: (0, 0),
    })
}

fn install_mask(layer: &mut crate::core::layer::Layer, mask: crate::core::layer::LayerMask) {
    layer.mask = Some(mask);
    layer.mask_active = true;
    layer.mask_linked = true;
    layer.paint_target = crate::core::layer::PaintTarget::Mask;
}

/// Write the refined selection to the layers. The new-layer outputs copy the
/// active layer above it and hide the original, as Photoshop does. Returns
/// false when nothing could be written.
fn output_refined_selection(
    canvas: &mut crate::core::canvas::Canvas,
    sel: &crate::core::selection::Selection,
    mode: RefineOutputMode,
    decontaminate: Option<f32>,
) -> bool {
    let (cw, ch) = (canvas.width, canvas.height);
    if sel.mask.len() < cw as usize * ch as usize {
        return false;
    }
    canvas.layer_stack.normalize_active_idx();
    let idx = canvas.layer_stack.active_idx;
    let Some(src) = canvas.layer_stack.layers.get(idx) else {
        return false;
    };
    match mode {
        RefineOutputMode::Selection => false,
        RefineOutputMode::LayerMask => {
            let Some(mask) = layer_mask_from_selection(sel, src, cw, ch) else {
                return false;
            };
            install_mask(&mut canvas.layer_stack.layers[idx], mask);
            true
        }
        RefineOutputMode::NewLayer | RefineOutputMode::NewLayerWithMask => {
            // Cutting the pixels needs a raster layer; others keep a mask.
            let with_mask = mode == RefineOutputMode::NewLayerWithMask || !src.is_raster();
            let new_idx = canvas.layer_stack.duplicate_layer(idx);
            if new_idx == idx {
                return false;
            }
            let layer = &mut canvas.layer_stack.layers[new_idx];
            if with_mask {
                let Some(mask) = layer_mask_from_selection(sel, layer, cw, ch) else {
                    return false;
                };
                install_mask(layer, mask);
            } else {
                super::clipboard::mask_copied_layer_by_selection(layer, sel, cw, ch);
            }
            if let Some(amount) = decontaminate {
                decontaminate_layer(layer, sel, cw, ch, amount);
            }
            canvas.layer_stack.layers[idx].visible = false;
            canvas.layer_stack.active_idx = new_idx;
            true
        }
    }
}

/// Decontaminate Colors: edge pixels take on the colour of nearby fully
/// selected pixels, the more the softer the selection is there.
fn decontaminate_layer(
    layer: &mut crate::core::layer::Layer,
    sel: &crate::core::selection::Selection,
    cw: u32,
    ch: u32,
    amount: f32,
) {
    use crate::core::tile::TILE_SIZE;
    const SEARCH_R: i64 = 8;
    let amount = amount.clamp(0.0, 1.0);
    if amount <= 0.001 {
        return;
    }
    let (cw, ch) = (cw as i64, ch as i64);
    let mask_at = |x: i64, y: i64| -> u8 {
        if x >= 0 && y >= 0 && x < cw && y < ch {
            sel.mask[(y * cw + x) as usize]
        } else {
            0
        }
    };
    let (ox, oy) = (layer.offset.0 as i64, layer.offset.1 as i64);
    let source = layer.tiles.clone();
    let (lw, lh) = (layer.width as i64, layer.height as i64);
    let positions: Vec<_> = layer.tiles.tiles.keys().copied().collect();
    for pos in positions {
        let Some(tile) = layer.tiles.tiles.get_mut(&pos) else {
            continue;
        };
        let tile = std::sync::Arc::make_mut(tile);
        let mut touched = false;
        for ty in 0..TILE_SIZE as i64 {
            for tx in 0..TILE_SIZE as i64 {
                let (lx, ly) = (
                    pos.x as i64 * TILE_SIZE as i64 + tx,
                    pos.y as i64 * TILE_SIZE as i64 + ty,
                );
                let (cx, cy) = (lx + ox, ly + oy);
                let m = mask_at(cx, cy);
                let i = ((ty * TILE_SIZE as i64 + tx) * 4) as usize;
                if m == 0 || tile.pixels[i + 3] == 0 {
                    continue;
                }
                let near_edge = m < 255
                    || mask_at(cx - 1, cy) < 245
                    || mask_at(cx + 1, cy) < 245
                    || mask_at(cx, cy - 1) < 245
                    || mask_at(cx, cy + 1) < 245;
                if !near_edge {
                    continue;
                }
                let mut sum = [0u64; 3];
                let mut count = 0u64;
                for sy in -SEARCH_R..=SEARCH_R {
                    for sx in -SEARCH_R..=SEARCH_R {
                        if sx * sx + sy * sy > SEARCH_R * SEARCH_R
                            || mask_at(cx + sx, cy + sy) < 220
                        {
                            continue;
                        }
                        let (sx, sy) = (lx + sx, ly + sy);
                        if sx < 0 || sy < 0 || sx >= lw || sy >= lh {
                            continue;
                        }
                        let (r, g, b, a) = source.get_pixel16(sx as u32, sy as u32);
                        if a == 0 {
                            continue;
                        }
                        sum[0] += r as u64;
                        sum[1] += g as u64;
                        sum[2] += b as u64;
                        count += 1;
                    }
                }
                if count == 0 {
                    continue;
                }
                let mix = (amount * (1.0 - m as f32 / 255.0).sqrt()).clamp(0.0, 1.0);
                let (r, g, b, _) = source.get_pixel16(lx as u32, ly as u32);
                let cur = [r, g, b];
                for c in 0..3 {
                    let fg = sum[c] as f32 / count as f32;
                    let v16 = (cur[c] as f32 * (1.0 - mix) + fg * mix)
                        .round()
                        .clamp(0.0, 65535.0);
                    tile.pixels[i + c] = (v16 / 257.0).round() as u8;
                    if let Some(p16) = tile.pixels16.as_mut() {
                        p16[i + c] = v16 as u16;
                    }
                }
                touched = true;
            }
        }
        if touched {
            tile.revision = crate::core::tile::next_tile_revision();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::selection::RefineBrushMode;
    use crate::tools::{PointerEvent, ToolCtx};

    fn app_with_selection() -> App {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.docs.documents[0].canvas = Canvas::new(200, 160);
        app.docs.documents[0]
            .canvas
            .selection
            .select_rect(40, 30, 120, 110);
        app
    }

    fn canvas(app: &App) -> &Canvas {
        &app.docs.documents[app.docs.active_doc_idx].canvas
    }

    fn stroke(app: &mut App, mode: RefineBrushMode, points: &[(f32, f32)]) {
        app.edit.tools.refine_brush_mut().mode = mode;
        app.edit.tools.refine_brush_mut().size = 20.0;
        let doc = app.docs.active_doc_idx;
        let mut ctx = ToolCtx::new(
            &mut app.docs.documents[doc],
            [0; 4],
            [255; 4],
            1.0,
            0.0,
            0.0,
        );
        let first = points[0];
        let _ = app
            .edit
            .tools
            .on_press(PointerEvent::new(first.0, first.1), &mut ctx);
        for &(x, y) in &points[1..] {
            let _ = app.edit.tools.on_drag(PointerEvent::new(x, y), &mut ctx);
        }
        let _ = app.edit.tools.on_frame(&mut ctx);
        let last = points[points.len() - 1];
        let _ = app
            .edit
            .tools
            .on_release(PointerEvent::new(last.0, last.1), &mut ctx);
    }

    fn feather(app: &mut App, px: f32) {
        let p = RefineParams {
            feather: px,
            ..app.refine_params()
        };
        app.set_refine_params(Some(p), true);
    }

    #[test]
    fn brush_and_sliders_work_together() {
        let mut app = app_with_selection();
        app.open_refine_panel();
        assert_eq!(
            app.edit.tools.active_id(),
            crate::tools::ToolId::RefineBrush
        );
        feather(&mut app, 3.0);
        stroke(
            &mut app,
            RefineBrushMode::Add,
            &[(160.0, 70.0), (170.0, 70.0)],
        );
        // The stroke shows, and the feather set before it is still there.
        let sel = &canvas(&app).selection;
        assert!(sel.mask[70 * 200 + 165] > 200, "stroke painted");
        let soft = sel.mask.iter().filter(|&&v| v > 0 && v < 255).count();
        assert!(soft > 100, "feather kept after the stroke: {soft}");
        assert!((app.refine_params().feather - 3.0).abs() < 1e-6);
        // Moving a slider keeps the stroke.
        feather(&mut app, 1.0);
        assert!(
            canvas(&app).selection.mask[70 * 200 + 165] > 200,
            "stroke kept"
        );
    }

    #[test]
    fn cancel_restores_the_selection_and_records_nothing() {
        let mut app = app_with_selection();
        let before = canvas(&app).selection.mask.clone();
        let undo_before = canvas(&app).undo_count();
        app.edit.tools.select(crate::tools::ToolId::Lasso);
        app.open_refine_panel();
        stroke(
            &mut app,
            RefineBrushMode::Add,
            &[(160.0, 70.0), (180.0, 90.0)],
        );
        stroke(&mut app, RefineBrushMode::Subtract, &[(60.0, 60.0)]);
        feather(&mut app, 4.0);
        assert_ne!(canvas(&app).selection.mask, before);
        app.cancel_refine_panel();
        assert_eq!(canvas(&app).selection.mask, before);
        assert_eq!(canvas(&app).undo_count(), undo_before);
        assert!(canvas(&app).refine.is_none());
        assert!(!app.edit.show_refine_panel);
        assert_eq!(app.edit.tools.active_id(), crate::tools::ToolId::Lasso);
    }

    #[test]
    fn ok_is_one_undo_step_back_to_the_start() {
        let mut app = app_with_selection();
        let before = canvas(&app).selection.mask.clone();
        let undo_before = canvas(&app).undo_count();
        app.open_refine_panel();
        stroke(
            &mut app,
            RefineBrushMode::Add,
            &[(160.0, 70.0), (180.0, 90.0)],
        );
        stroke(&mut app, RefineBrushMode::Subtract, &[(60.0, 60.0)]);
        feather(&mut app, 2.0);
        let refined = canvas(&app).selection.mask.clone();
        app.commit_refine_panel();
        assert_eq!(canvas(&app).selection.mask, refined);
        assert_eq!(canvas(&app).undo_count(), undo_before + 1);
        let doc = app.docs.active_doc_idx;
        app.docs.documents[doc].canvas.undo();
        assert_eq!(canvas(&app).selection.mask, before);
    }

    #[test]
    fn ctrl_z_in_the_panel_steps_back_one_stroke() {
        let mut app = app_with_selection();
        app.open_refine_panel();
        stroke(&mut app, RefineBrushMode::Add, &[(160.0, 70.0)]);
        let after_first = canvas(&app).selection.mask.clone();
        stroke(&mut app, RefineBrushMode::Add, &[(160.0, 130.0)]);
        assert_ne!(canvas(&app).selection.mask, after_first);
        app.refine_history_step(false);
        assert_eq!(canvas(&app).selection.mask, after_first);
        app.refine_history_step(true);
        assert_ne!(canvas(&app).selection.mask, after_first);
    }

    #[test]
    fn layer_mask_output_lines_up_with_a_moved_layer_and_undoes() {
        let mut app = app_with_selection();
        let before_sel = canvas(&app).selection.mask.clone();
        {
            let c = &mut app.docs.documents[0].canvas;
            let idx = c.layer_stack.add_layer(100, 80);
            c.layer_stack.layers[idx].offset = (30, 20);
            c.layer_stack.active_idx = idx;
        }
        app.open_refine_panel();
        app.edit.refine_output_mode = RefineOutputMode::LayerMask;
        app.commit_refine_panel();
        let c = canvas(&app);
        let layer = &c.layer_stack.layers[c.layer_stack.active_idx];
        let mask = layer.mask.as_ref().expect("mask added");
        assert_eq!((mask.width, mask.height), (100, 80));
        // Layer pixel (lx, ly) sits on canvas (lx + 30, ly + 20).
        assert_eq!(
            mask.tiles.get_pixel(15, 15).0,
            255,
            "canvas (45,35) is selected"
        );
        assert_eq!(mask.tiles.get_pixel(5, 5).0, 0, "canvas (35,25) is not");
        assert!(!c.selection.active, "the selection is used up");
        let doc = app.docs.active_doc_idx;
        app.docs.documents[doc].canvas.undo();
        let c = canvas(&app);
        assert!(c.layer_stack.layers[c.layer_stack.active_idx]
            .mask
            .is_none());
        assert_eq!(c.selection.mask, before_sel);
    }

    #[test]
    fn new_layer_output_hides_the_source_and_undoes() {
        let mut app = app_with_selection();
        let layers_before = canvas(&app).layer_stack.layers.len();
        app.open_refine_panel();
        app.edit.refine_output_mode = RefineOutputMode::NewLayerWithMask;
        app.commit_refine_panel();
        let c = canvas(&app);
        assert_eq!(c.layer_stack.layers.len(), layers_before + 1);
        assert!(!c.layer_stack.layers[c.layer_stack.active_idx - 1].visible);
        assert!(c.layer_stack.layers[c.layer_stack.active_idx]
            .mask
            .is_some());
        let doc = app.docs.active_doc_idx;
        app.docs.documents[doc].canvas.undo();
        let c = canvas(&app);
        assert_eq!(c.layer_stack.layers.len(), layers_before);
        assert!(c.layer_stack.layers.iter().all(|l| l.visible));
    }
}
