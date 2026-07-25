//! Fill / Outline styling of the active Path layer (options-bar controls under
//! the Move and Node tools). The vector command [`ChangeVectorStyle`] already
//! exists; this is the App glue that reads the active Path's style for the UI and
//! applies edits through the gateway.
//!
//! Two kinds of edit:
//!   * discrete toggles (Fill on/off, Outline on/off) commit ONE
//!     `ChangeVectorStyle` immediately;
//!   * interactive scrubs (colour dialog drag, outline-width DragValue) preview
//!     live on the model without touching history, capture a baseline at the
//!     start, and commit ONE `ChangeVectorStyle` when the interaction ends —
//!     mirroring the opacity-slider pattern so a scrub is a single undo step.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::layer::LayerType;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::color::ColorValue;
use crate::core::vector::object::VectorGeometry;
use crate::core::vector::style::{
    DashPattern, Gradient, GradientKind, GradientStop, Paint, VectorStyle, MAX_DASHES,
    MAX_GRADIENT_STOPS,
};

impl App {
    /// Context-sensitive Gradient Tool target:
    /// 0 disabled, 1 raster pixels, 2 vector object, 3 layer mask.
    pub fn active_gradient_mode(&self) -> u8 {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(layer) = canvas.layer_stack.layers.get(canvas.layer_stack.active_idx) else {
            return 0;
        };
        if !layer.visible || (layer.locked && !layer.is_background) {
            return 0;
        }
        if layer.paint_target == crate::core::layer::PaintTarget::Mask {
            return u8::from(layer.mask.is_some()) * 3;
        }
        match layer.layer_type {
            LayerType::Raster => 1,
            LayerType::Vector(_) if !layer.is_background => 2,
            _ => 0,
        }
    }

    pub fn active_gradient_ui_type(&self) -> u8 {
        if self.active_gradient_mode() == 2 {
            if let Some((_, style)) = self.active_path_style() {
                if let Paint::Gradient(gradient) = style.fill {
                    return u8::from(gradient.kind == GradientKind::Radial);
                }
            }
            return u8::from(
                self.edit.tools.gradient().gradient_type
                    == crate::tools::gradient::GradientType::Radial,
            );
        }
        self.edit.tools.gradient().gradient_type as u8
    }

    pub fn active_gradient_ui_stops(&self) -> Vec<(f32, [u8; 4])> {
        if self.active_gradient_mode() == 2 {
            if let Some((_, style)) = self.active_path_style() {
                if let Paint::Gradient(gradient) = style.fill {
                    return gradient
                        .active_stops()
                        .iter()
                        .map(|stop| (stop.offset, stop.color.to_rgba8()))
                        .collect();
                }
            }
        }
        self.edit
            .tools
            .gradient()
            .gradient
            .stops
            .iter()
            .map(|stop| (stop.position, stop.color))
            .collect()
    }

    /// Change the Vector target's gradient kind. A solid target only updates
    /// the tool preset; it is not modified until the first canvas drag.
    pub fn vector_gradient_set_kind(&mut self, kind: u8) -> bool {
        let Some((_, style)) = self.active_path_style() else {
            return false;
        };
        if !matches!(style.fill, Paint::Gradient(_)) {
            return false;
        }
        self.commit_path_style_change(|style| {
            if let Paint::Gradient(gradient) = &mut style.fill {
                gradient.kind = if kind == 1 {
                    GradientKind::Radial
                } else {
                    GradientKind::Linear
                };
            }
        });
        true
    }

    /// Live-preview all stops from the shared Gradient Editor. Returns false
    /// for a solid Vector target so the same editor can continue editing the
    /// working preset without changing the document before a drag.
    pub fn vector_gradient_set_stops(&mut self, stops: Vec<(f32, [u8; 4])>) -> bool {
        let Some((id, mut style)) = self.active_path_style() else {
            return false;
        };
        let Paint::Gradient(gradient) = &mut style.fill else {
            return false;
        };
        let old_stops = gradient.active_stops().to_vec();
        let mut stops = stops;
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        stops.truncate(MAX_GRADIENT_STOPS);
        if stops.len() < 2 {
            return true;
        }
        self.path_style_begin();
        gradient.stop_count = stops.len() as u8;
        let mut reused = [false; MAX_GRADIENT_STOPS];
        for (index, (offset, color)) in stops.into_iter().enumerate() {
            // The shared editor transports an RGBA preview. Reuse the exact
            // model colour when its preview is unchanged, so moving a CMYK
            // stop does not silently convert it to RGB.
            let same_index = old_stops
                .get(index)
                .filter(|stop| !reused[index] && stop.color.to_rgba8() == color)
                .map(|stop| (index, stop.color));
            let preserved = same_index.or_else(|| {
                old_stops.iter().enumerate().find_map(|(old_index, stop)| {
                    (!reused[old_index] && stop.color.to_rgba8() == color)
                        .then_some((old_index, stop.color))
                })
            });
            let model_color = if let Some((old_index, model_color)) = preserved {
                reused[old_index] = true;
                model_color
            } else {
                ColorValue::from_rgba8(color)
            };
            gradient.stops[index] = GradientStop {
                offset: offset.clamp(0.0, 1.0),
                color: model_color,
            };
        }
        self.preview_path_style_live(id, style);
        true
    }

    pub fn vector_gradient_reverse(&mut self) -> bool {
        let Some((_, style)) = self.active_path_style() else {
            return false;
        };
        if !matches!(style.fill, Paint::Gradient(_)) {
            return false;
        }
        self.commit_path_style_change(|style| {
            if let Paint::Gradient(gradient) = &mut style.fill {
                let count = gradient.stop_count as usize;
                for stop in &mut gradient.stops[..count] {
                    stop.offset = 1.0 - stop.offset;
                }
                gradient.stops[..count].reverse();
            }
        });
        true
    }

    /// `(layer_id, current style)` of the active editable Vector layer.
    pub(in crate::app) fn active_path_style(&self) -> Option<(u32, VectorStyle)> {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer = canvas
            .layer_stack
            .layers
            .get(canvas.layer_stack.active_idx)?;
        if !layer.visible || layer.locked || layer.is_background {
            return None;
        }
        match &layer.layer_type {
            LayerType::Vector(VectorGeometry::Path(o)) => Some((layer.id, o.style)),
            LayerType::Vector(VectorGeometry::Primitive(shape)) => Some((layer.id, shape.style)),
            _ => None,
        }
    }

    fn vector_style_by_id(&self, layer_id: u32) -> Option<VectorStyle> {
        let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let layer = canvas
            .layer_stack
            .layers
            .iter()
            .find(|layer| layer.id == layer_id)?;
        match &layer.layer_type {
            LayerType::Vector(VectorGeometry::Path(object)) => Some(object.style),
            LayerType::Vector(VectorGeometry::Primitive(shape)) => Some(shape.style),
            _ => None,
        }
    }

    /// Fill/outline snapshot for the options-bar controls, or `None` when the
    /// active layer isn't an editable Path.
    pub fn active_path_style_vm(&self) -> Option<crate::ui::PathStyleData> {
        let (_, style) = self.active_path_style()?;
        let mut gradient_stop_colors = [[0, 0, 0, 255]; MAX_GRADIENT_STOPS];
        let mut gradient_stop_offsets = [0.0; MAX_GRADIENT_STOPS];
        let gradient_stop_count = if let Paint::Gradient(g) = style.fill {
            for (index, stop) in g.active_stops().iter().enumerate() {
                gradient_stop_colors[index] = stop.color.to_rgba8();
                gradient_stop_offsets[index] = stop.offset;
            }
            g.stop_count
        } else {
            0
        };
        let color_of = |p: Paint, fallback: [u8; 4]| match p {
            Paint::Solid(c) => c.to_rgba8(),
            Paint::Gradient(g) => g
                .active_stops()
                .first()
                .map_or(fallback, |s| s.color.to_rgba8()),
            Paint::None => fallback,
        };
        Some(crate::ui::PathStyleData {
            fill_enabled: style.fill.is_visible(),
            fill_color: color_of(style.fill, [0, 0, 0, 255]),
            fill_value: match style.fill {
                Paint::Solid(color) => Some(color),
                Paint::Gradient(gradient) => gradient.active_stops().first().map(|stop| stop.color),
                Paint::None => None,
            },
            fill_overprint: style.fill_overprint,
            fill_end_color: match style.fill {
                Paint::Gradient(g) => g
                    .active_stops()
                    .last()
                    .map_or([255, 255, 255, 255], |s| s.color.to_rgba8()),
                _ => [255, 255, 255, 255],
            },
            gradient_stop_colors,
            gradient_stop_offsets,
            gradient_stop_count,
            stroke_enabled: style.stroke.is_visible(),
            stroke_color: color_of(style.stroke, [0, 0, 0, 255]),
            stroke_value: match style.stroke {
                Paint::Solid(color) => Some(color),
                Paint::Gradient(gradient) => gradient.active_stops().first().map(|stop| stop.color),
                Paint::None => None,
            },
            stroke_overprint: style.stroke_overprint,
            stroke_width: style.stroke_style.width,
            fill_kind: match style.fill {
                Paint::Gradient(g) if g.kind == GradientKind::Linear => 1,
                Paint::Gradient(_) => 2,
                _ => 0,
            },
            dash_kind: {
                let d = style.stroke_style.dash.as_slice();
                if d.is_empty() {
                    0
                } else if d.len() == 2 && d[0] <= d[1] * 0.35 {
                    2
                } else {
                    1
                }
            },
            dash_values: style.stroke_style.dash.values,
            dash_len: style.stroke_style.dash.len,
            dash_offset: style.stroke_style.dash.offset,
        })
    }

    /// Re-raster the active Path with `style`, WITHOUT recording history (a live
    /// preview). Dirty-rect invalidation only.
    fn preview_path_style(&mut self, layer_id: u32, style: VectorStyle) -> bool {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return false;
        };
        let (old_off, old_w, old_h) = {
            let layer = &canvas.layer_stack.layers[idx];
            if !matches!(layer.layer_type, LayerType::Vector(_)) {
                return false;
            }
            (layer.offset, layer.width, layer.height)
        };
        {
            let layer = &mut canvas.layer_stack.layers[idx];
            if crate::core::command_vector::apply_style_to_layer(layer, style).is_err() {
                return false;
            }
        }
        canvas.layer_revision += 1;
        let (new_off, new_w, new_h) = {
            let l = &canvas.layer_stack.layers[idx];
            (l.offset, l.width, l.height)
        };
        canvas.mark_dirty_layer_bounds(old_off.0, old_off.1, old_w, old_h);
        canvas.mark_dirty_layer_bounds(new_off.0, new_off.1, new_w, new_h);
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        true
    }

    /// Live-preview `style` on the active Path WITHOUT recording history, the way
    /// a colour-dialog drag or width scrub should: the model style is updated
    /// immediately (cheap — so a following read/commit sees the latest), but the
    /// expensive fill re-raster is handed to the OFF-THREAD Path bake. The old
    /// path re-rastered synchronously on the UI thread on every colour tick, which
    /// stalled the picker on a big filled path ("fill màu lag"). Mirrors the
    /// live scale/rotate drag, which bakes off-thread for the same reason.
    fn preview_path_style_live(&mut self, layer_id: u32, style: VectorStyle) {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(idx) = canvas
            .layer_stack
            .layers
            .iter()
            .position(|l| l.id == layer_id)
        else {
            return;
        };
        let path_object = {
            let layer = &mut canvas.layer_stack.layers[idx];
            match &mut layer.layer_type {
                LayerType::Vector(VectorGeometry::Path(object)) => {
                    object.style = style;
                    Some(object.clone())
                }
                LayerType::Vector(VectorGeometry::Primitive(_)) => None,
                _ => return,
            }
        };
        if let Some(object) = path_object {
            self.request_path_bake(layer_id, object);
        } else {
            let _ = self.preview_path_style(layer_id, style);
        }
    }

    /// Update only the vector appearance model during an on-canvas gradient
    /// handle drag. The control overlay follows this immediately, while the
    /// potentially page-sized raster cache is rebuilt once on release.
    fn preview_path_style_model_only(&mut self, layer_id: u32, style: VectorStyle) {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(layer) = canvas
            .layer_stack
            .layers
            .iter_mut()
            .find(|layer| layer.id == layer_id)
        else {
            return;
        };
        match &mut layer.layer_type {
            LayerType::Vector(VectorGeometry::Path(object)) => object.style = style,
            LayerType::Vector(VectorGeometry::Primitive(shape)) => shape.style = style,
            _ => return,
        }
        if let Some(window) = &self.win.window {
            window.request_redraw();
        }
    }

    /// Capture the pre-edit style baseline for the active Path (once per
    /// interaction), so the scrub/dialog commits as one undo step.
    fn path_style_begin(&mut self) {
        let Some((id, style)) = self.active_path_style() else {
            return;
        };
        if self
            .edit
            .pending_path_style
            .is_some_and(|(pending_id, _)| pending_id != id)
        {
            // Finish the pinned old target before starting a session on the
            // newly selected vector layer. Commands use layer ids, so a panel
            // selection change can never redirect the edit.
            self.path_style_commit();
        }
        if self.edit.pending_path_style.is_none() {
            self.edit.pending_path_style = Some((id, style));
        }
    }

    /// Commit an interactive Path style edit: rewind the model to the captured
    /// baseline, then record ONE `ChangeVectorStyle` for the previewed style.
    pub fn path_style_commit(&mut self) {
        let Some((id, baseline)) = self.edit.pending_path_style.take() else {
            return;
        };
        // Drop any in-flight / queued live-preview bake: the final style is
        // rasterised synchronously below, so a late worker result would be stale
        // (same guard the transform commit uses).
        self.cancel_path_bake();
        let Some(final_style) = self.vector_style_by_id(id) else {
            return;
        };
        if final_style == baseline {
            return;
        }
        if final_style.validate().is_err() {
            let _ = self.preview_path_style(id, baseline);
            return;
        }
        // A crisp-display job may still carry the pre-drag vector style, and
        // its suppression state may be hiding the document-resolution layer.
        // Remove both before rebuilding the committed raster so the very next
        // composite is sourced only from the final model.
        self.invalidate_vector_display();
        // The preview already owns the final model. Rebuild its cache once,
        // then record a command carrying the captured baseline directly.
        if !self.preview_path_style(id, final_style) {
            let _ = self.preview_path_style(id, baseline);
            return;
        }
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        canvas.reconcile_path_ink();
        let _ = canvas.record_as(
            Box::new(
                crate::core::command_vector::ChangeVectorStyle::already_applied(
                    id,
                    baseline,
                    final_style,
                ),
            ),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Apply a discrete style change (Fill/Outline on-off) as ONE undo step.
    fn commit_path_style_change(&mut self, apply: impl FnOnce(&mut VectorStyle)) {
        let Some((id, mut style)) = self.active_path_style() else {
            return;
        };
        let before = style;
        apply(&mut style);
        if style == before {
            return;
        }
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let _ = canvas.execute(
            Box::new(crate::core::command_vector::ChangeVectorStyle::new(
                id, style,
            )),
            crate::core::gateway::ChangeKind::LayerStructure,
        );
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }

    /// Toggle the Path's fill on/off (discrete, one undo). Enabling uses black
    /// when the fill had no colour to fall back to.
    pub fn path_set_fill_enabled(&mut self, on: bool) {
        self.commit_path_style_change(|s| {
            if on {
                let c = match s.fill {
                    Paint::Solid(c) => c,
                    Paint::Gradient(g) => g.active_stops()[0].color,
                    Paint::None => ColorValue::BLACK,
                };
                s.fill = Paint::Solid(c);
            } else {
                s.fill = Paint::None;
                s.fill_overprint = false;
            }
        });
    }

    /// Toggle the Path's outline on/off (discrete, one undo). Enabling picks a
    /// sensible default width when the outline was zero-width.
    pub fn path_set_stroke_enabled(&mut self, on: bool) {
        self.commit_path_style_change(|s| {
            if on {
                let c = match s.stroke {
                    Paint::Solid(c) => c,
                    Paint::Gradient(g) => g.active_stops()[0].color,
                    Paint::None => ColorValue::BLACK,
                };
                s.stroke = Paint::Solid(c);
                if s.stroke_style.width <= 0.0 {
                    s.stroke_style.width = 1.0;
                }
            } else {
                s.stroke = Paint::None;
                s.stroke_overprint = false;
            }
        });
    }

    /// Apply an exact process colour from the document/quick palette. Palette
    /// clicks deliberately choose a solid paint (matching Corel's palette
    /// gesture); gradient-stop editing remains in the gradient UI.
    pub fn path_apply_palette_fill(&mut self, color: ColorValue) -> bool {
        if self.active_path_style().is_none() {
            return false;
        }
        self.path_style_commit();
        self.commit_path_style_change(|style| style.fill = Paint::Solid(color));
        true
    }

    pub fn path_apply_palette_outline(&mut self, color: ColorValue) -> bool {
        if self.active_path_style().is_none() {
            return false;
        }
        self.path_style_commit();
        self.commit_path_style_change(|style| {
            style.stroke = Paint::Solid(color);
            if style.stroke_style.width <= 0.0 {
                style.stroke_style.width = 1.0;
            }
        });
        true
    }

    pub fn path_clear_palette_fill(&mut self) -> bool {
        if self.active_path_style().is_none() {
            return false;
        }
        self.path_style_commit();
        self.commit_path_style_change(|style| {
            style.fill = Paint::None;
            style.fill_overprint = false;
        });
        true
    }

    pub fn path_clear_palette_outline(&mut self) -> bool {
        if self.active_path_style().is_none() {
            return false;
        }
        self.path_style_commit();
        self.commit_path_style_change(|style| {
            style.stroke = Paint::None;
            style.stroke_overprint = false;
        });
        true
    }

    pub fn path_set_fill_overprint(&mut self, enabled: bool) {
        self.path_style_commit();
        self.commit_path_style_change(|style| {
            style.fill_overprint = enabled && style.fill.is_visible();
        });
    }

    pub fn path_set_stroke_overprint(&mut self, enabled: bool) {
        self.path_style_commit();
        self.commit_path_style_change(|style| {
            style.stroke_overprint = enabled && style.stroke.is_visible();
        });
    }

    /// Live-preview the Path's fill colour during a colour-dialog drag (call
    /// [`Self::path_style_commit`] on OK).
    pub fn path_set_fill_color(&mut self, rgba: [u8; 4]) {
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            match &mut style.fill {
                Paint::Gradient(g) => g.stops[0].color = ColorValue::from_rgba8(rgba),
                _ => style.fill = Paint::Solid(ColorValue::from_rgba8(rgba)),
            }
            self.preview_path_style_live(id, style);
        }
    }

    pub fn path_set_fill_end_color(&mut self, rgba: [u8; 4]) {
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            if let Paint::Gradient(g) = &mut style.fill {
                let last = g.stop_count.saturating_sub(1) as usize;
                g.stops[last].color = ColorValue::from_rgba8(rgba);
                self.preview_path_style_live(id, style);
            }
        }
    }

    /// Live-preview one vector-gradient stop colour. The colour dialog calls
    /// `path_style_commit` on OK/Cancel so the whole interaction is one undo.
    pub fn path_set_gradient_stop_color(&mut self, index: u8, rgba: [u8; 4]) {
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            if let Paint::Gradient(g) = &mut style.fill {
                let index = index as usize;
                if index < g.stop_count as usize {
                    g.stops[index].color = ColorValue::from_rgba8(rgba);
                    self.preview_path_style_live(id, style);
                }
            }
        }
    }

    /// Live-preview a stop location while preserving sorted gradient order.
    pub fn path_set_gradient_stop_offset(&mut self, index: u8, offset: f32) {
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            if let Paint::Gradient(g) = &mut style.fill {
                let index = index as usize;
                let count = g.stop_count.min(MAX_GRADIENT_STOPS as u8) as usize;
                if index < count {
                    let lo = if index == 0 {
                        0.0
                    } else {
                        g.stops[index - 1].offset
                    };
                    let hi = if index + 1 == count {
                        1.0
                    } else {
                        g.stops[index + 1].offset
                    };
                    g.stops[index].offset = if offset.is_finite() {
                        offset.clamp(lo, hi)
                    } else {
                        lo
                    };
                    self.preview_path_style_live(id, style);
                }
            }
        }
    }

    /// Live-preview an on-canvas edit of the gradient's local transform.
    pub(in crate::app) fn path_set_gradient_transform(&mut self, transform: AffineTransform) {
        if !transform.is_finite() || transform.inverse().is_none() {
            return;
        }
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            if let Paint::Gradient(g) = &mut style.fill {
                g.transform = transform;
                self.preview_path_style_model_only(id, style);
            }
        }
    }

    /// Insert a colour stop at the midpoint of the widest interval.
    pub fn path_add_gradient_stop(&mut self) {
        self.path_style_commit();
        self.commit_path_style_change(|style| {
            let Paint::Gradient(g) = &mut style.fill else {
                return;
            };
            let count = g.stop_count.min(MAX_GRADIENT_STOPS as u8) as usize;
            if !(2..MAX_GRADIENT_STOPS).contains(&count) {
                return;
            }
            let (left, _) = g.active_stops().windows(2).enumerate().fold(
                (0usize, -1.0_f32),
                |best, (index, pair)| {
                    let gap = pair[1].offset - pair[0].offset;
                    if gap > best.1 {
                        (index, gap)
                    } else {
                        best
                    }
                },
            );
            let right = left + 1;
            let offset = (g.stops[left].offset + g.stops[right].offset) * 0.5;
            let a = g.stops[left].color.to_rgba8();
            let b = g.stops[right].color.to_rgba8();
            let midpoint =
                std::array::from_fn(|channel| ((a[channel] as u16 + b[channel] as u16) / 2) as u8);
            for index in (right..count).rev() {
                g.stops[index + 1] = g.stops[index];
            }
            g.stops[right] = GradientStop {
                offset,
                color: ColorValue::from_rgba8(midpoint),
            };
            g.stop_count = (count + 1) as u8;
        });
    }

    /// Remove one gradient stop while retaining the model minimum of two.
    pub fn path_remove_gradient_stop(&mut self, index: u8) {
        self.path_style_commit();
        self.commit_path_style_change(|style| {
            let Paint::Gradient(g) = &mut style.fill else {
                return;
            };
            let count = g.stop_count.min(MAX_GRADIENT_STOPS as u8) as usize;
            let index = index as usize;
            if count <= 2 || index >= count {
                return;
            }
            for current in index..count - 1 {
                g.stops[current] = g.stops[current + 1];
            }
            g.stops[count - 1] = GradientStop::default();
            g.stop_count = (count - 1) as u8;
        });
    }

    /// Live-preview the Path's outline colour during a colour-dialog drag.
    pub fn path_set_stroke_color(&mut self, rgba: [u8; 4]) {
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            style.stroke = Paint::Solid(ColorValue::from_rgba8(rgba));
            if style.stroke_style.width <= 0.0 {
                style.stroke_style.width = 1.0;
            }
            self.preview_path_style_live(id, style);
        }
    }

    /// Live-preview the Path's outline width during a DragValue scrub (call
    /// [`Self::path_style_commit`] when the scrub ends).
    pub fn path_set_stroke_width(&mut self, w: f32) {
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            style.stroke_style.width = w.clamp(0.0, 500.0);
            // A width scrub implies a visible outline.
            if w > 0.0 && matches!(style.stroke, Paint::None) {
                style.stroke = Paint::Solid(ColorValue::BLACK);
            }
            self.preview_path_style_live(id, style);
        }
    }

    pub fn path_set_dash_kind(&mut self, kind: u8) {
        self.commit_path_style_change(|s| {
            let w = s.stroke_style.width.max(1.0);
            s.stroke_style.dash = match kind {
                1 => DashPattern::from_slice(&[4.0 * w, 2.0 * w], 0.0),
                2 => DashPattern::from_slice(&[0.25 * w, 1.75 * w], 0.0),
                _ => DashPattern::default(),
            };
            if kind != 0 && matches!(s.stroke, Paint::None) {
                s.stroke = Paint::Solid(ColorValue::BLACK);
            }
        });
    }

    /// Live-preview a custom dash/gap array. Invalid UI values are normalised
    /// here as a final guard so the model never receives an invalid length.
    pub fn path_set_dash_values(&mut self, mut values: [f32; MAX_DASHES], len: u8) {
        self.path_style_begin();
        let len = len.clamp(1, MAX_DASHES as u8);
        for value in &mut values[..len as usize] {
            *value = if value.is_finite() {
                value.clamp(0.01, 10_000.0)
            } else {
                1.0
            };
        }
        if let Some((id, mut style)) = self.active_path_style() {
            style.stroke_style.dash =
                DashPattern::from_slice(&values[..len as usize], style.stroke_style.dash.offset);
            if matches!(style.stroke, Paint::None) {
                style.stroke = Paint::Solid(ColorValue::BLACK);
            }
            self.preview_path_style_live(id, style);
        }
    }

    /// Live-preview the phase of the current dash array.
    pub fn path_set_dash_offset(&mut self, offset: f32) {
        self.path_style_begin();
        if let Some((id, mut style)) = self.active_path_style() {
            style.stroke_style.dash.offset = if offset.is_finite() {
                offset.clamp(-10_000.0, 10_000.0)
            } else {
                0.0
            };
            self.preview_path_style_live(id, style);
        }
    }

    pub fn path_set_fill_kind(&mut self, kind: u8) {
        let Some(idx) = self.active_path_layer() else {
            return;
        };
        let object = match &self.docs.documents[self.docs.active_doc_idx]
            .canvas
            .layer_stack
            .layers[idx]
            .layer_type
        {
            LayerType::Vector(VectorGeometry::Path(o)) => o,
            _ => return,
        };
        let Some(bounds) = object.local_bounds(0.25) else {
            return;
        };
        let current = match object.style.fill {
            Paint::Solid(c) => c,
            Paint::Gradient(g) => g
                .active_stops()
                .first()
                .map_or(ColorValue::BLACK, |s| s.color),
            Paint::None => ColorValue::BLACK,
        };
        let end = ColorValue::WHITE;
        let paint = match kind {
            1 => Paint::Gradient(Gradient::two_color(
                GradientKind::Linear,
                current,
                end,
                AffineTransform {
                    a: bounds.w.max(1.0),
                    d: bounds.h.max(1.0),
                    e: bounds.x,
                    f: bounds.y,
                    ..AffineTransform::IDENTITY
                },
            )),
            2 => {
                let radius = (bounds.w.max(bounds.h) * 0.5).max(1.0);
                Paint::Gradient(Gradient::two_color(
                    GradientKind::Radial,
                    current,
                    end,
                    AffineTransform {
                        a: radius,
                        d: radius,
                        e: bounds.x + bounds.w * 0.5,
                        f: bounds.y + bounds.h * 0.5,
                        ..AffineTransform::IDENTITY
                    },
                ))
            }
            _ => Paint::Solid(current),
        };
        self.commit_path_style_change(|s| s.fill = paint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::canvas::Canvas;
    use crate::core::command_vector::CreatePathLayer;
    use crate::core::gateway::ChangeKind;
    use crate::core::geometry::Point;
    use crate::core::vector::affine::AffineTransform;
    use crate::core::vector::object::VectorObjectData;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};

    fn app_with_path() -> (App, u32) {
        let mut app = App::new();
        app.docs.documents[0].canvas = Canvas::new(200, 200);
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(0.0, 0.0)),
                    Node::sharp(Point::new(40.0, 0.0)),
                    Node::sharp(Point::new(40.0, 40.0)),
                    Node::sharp(Point::new(0.0, 40.0)),
                ],
                true,
            )],
            FillRule::NonZero,
        );
        let obj = VectorObjectData::new(
            path,
            VectorStyle::filled(ColorValue::rgb(1.0, 0.0, 0.0)),
            AffineTransform::translate(50.0, 50.0),
        );
        let id = {
            let canvas = &mut app.docs.documents[0].canvas;
            canvas
                .execute(
                    Box::new(CreatePathLayer::new(obj, "Path 1")),
                    ChangeKind::LayerStructure,
                )
                .unwrap();
            let idx = canvas
                .layer_stack
                .layers
                .iter()
                .position(|l| matches!(l.layer_type, LayerType::Vector(VectorGeometry::Path(_))))
                .unwrap();
            canvas.layer_stack.active_idx = idx;
            canvas.layer_stack.layers[idx].id
        };
        (app, id)
    }

    fn style(app: &App, id: u32) -> VectorStyle {
        match &app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .layer_type
        {
            LayerType::Vector(VectorGeometry::Path(o)) => o.style,
            _ => panic!("not a path"),
        }
    }

    #[test]
    fn vm_reports_current_fill_and_outline() {
        let (app, _id) = app_with_path();
        let vm = app.active_path_style_vm().unwrap();
        assert!(vm.fill_enabled);
        assert_eq!(vm.fill_color, [255, 0, 0, 255]);
        assert!(!vm.stroke_enabled, "default path has no outline");
    }

    #[test]
    fn gradient_mode_tracks_vector_raster_and_active_mask() {
        let (mut app, id) = app_with_path();
        assert_eq!(app.active_gradient_mode(), 2);

        let canvas = &mut app.docs.documents[0].canvas;
        let raster = canvas.layer_stack.add_layer(200, 200);
        canvas.layer_stack.active_idx = raster;
        assert_eq!(app.active_gradient_mode(), 1);

        let path = app.docs.documents[0]
            .canvas
            .layer_stack
            .layers
            .iter()
            .position(|layer| layer.id == id)
            .unwrap();
        let canvas = &mut app.docs.documents[0].canvas;
        canvas.layer_stack.active_idx = path;
        canvas.layer_stack.layers[path].mask =
            Some(crate::core::layer::LayerMask::new_white(200, 200));
        canvas.layer_stack.layers[path].paint_target = crate::core::layer::PaintTarget::Mask;
        assert_eq!(app.active_gradient_mode(), 3);
    }

    #[test]
    fn shared_gradient_editor_stops_commit_as_one_vector_undo() {
        let (mut app, id) = app_with_path();
        app.path_set_fill_kind(1);
        let baseline = style(&app, id);

        assert!(app.vector_gradient_set_stops(vec![
            (0.0, [10, 20, 30, 255]),
            (0.4, [40, 50, 60, 200]),
            (1.0, [70, 80, 90, 255]),
        ]));
        assert!(app.vector_gradient_set_stops(vec![
            (0.0, [1, 2, 3, 255]),
            (0.75, [4, 5, 6, 128]),
            (1.0, [7, 8, 9, 255]),
        ]));
        app.path_style_commit();

        let Paint::Gradient(gradient) = style(&app, id).fill else {
            panic!("expected vector gradient");
        };
        assert_eq!(gradient.stop_count, 3);
        assert!((gradient.stops[1].offset - 0.75).abs() < 1e-3);
        app.docs.documents[0].canvas.undo().expect("one undo");
        assert_eq!(style(&app, id), baseline);
    }

    #[test]
    fn shared_gradient_editor_preserves_exact_cmyk_when_moving_stop() {
        let (mut app, id) = app_with_path();
        let pure_k = ColorValue::cmyk(0.0, 0.0, 0.0, 1.0);
        let rich_cmyk = ColorValue::cmyka(0.7, 0.2, 0.1, 0.05, 0.8);
        {
            let canvas = &mut app.docs.documents[0].canvas;
            let layer = canvas
                .layer_stack
                .layers
                .iter_mut()
                .find(|layer| layer.id == id)
                .unwrap();
            let LayerType::Vector(VectorGeometry::Path(object)) = &mut layer.layer_type else {
                panic!("expected path");
            };
            object.style.fill = Paint::Gradient(Gradient::two_color(
                GradientKind::Linear,
                pure_k,
                rich_cmyk,
                AffineTransform::IDENTITY,
            ));
        }

        let mut preview = app.active_gradient_ui_stops();
        preview[1].0 = 0.65;
        assert!(app.vector_gradient_set_stops(preview));
        app.path_style_commit();

        let Paint::Gradient(gradient) = style(&app, id).fill else {
            panic!("expected gradient");
        };
        assert_eq!(gradient.stops[0].color, pure_k);
        assert_eq!(gradient.stops[1].color, rich_cmyk);
        assert!((gradient.stops[1].offset - 0.65).abs() < 1e-6);
    }

    #[test]
    fn switching_document_commits_gradient_editor_to_original_canvas() {
        let (mut app, id) = app_with_path();
        app.path_set_fill_kind(1);
        let baseline = style(&app, id);
        assert!(app.vector_gradient_set_stops(vec![
            (0.0, [20, 40, 60, 255]),
            (1.0, [220, 180, 100, 255]),
        ]));

        app.open_new_doc_tab();
        assert_eq!(app.docs.active_doc_idx, 1);
        assert!(matches!(style(&app, id).fill, Paint::Gradient(_)));
        app.docs.documents[0].canvas.undo().expect("one undo");
        assert_eq!(style(&app, id), baseline);
    }

    #[test]
    fn set_fill_color_is_one_undo_step() {
        let (mut app, id) = app_with_path();
        // Simulate a colour-dialog interaction: live preview then commit.
        app.path_set_fill_color([0, 128, 255, 255]);
        app.path_style_commit();
        assert_eq!(
            style(&app, id).fill,
            Paint::Solid(ColorValue::from_rgba8([0, 128, 255, 255]))
        );
        app.docs.documents[0].canvas.undo().expect("undo");
        assert_eq!(
            style(&app, id).fill,
            Paint::Solid(ColorValue::rgb(1.0, 0.0, 0.0)),
            "one undo restores the original fill"
        );
    }

    #[test]
    fn toggle_fill_off_and_undo() {
        let (mut app, id) = app_with_path();
        app.path_set_fill_enabled(false);
        assert_eq!(style(&app, id).fill, Paint::None);
        app.docs.documents[0].canvas.undo().expect("undo");
        assert!(matches!(style(&app, id).fill, Paint::Solid(_)));
    }

    #[test]
    fn stroke_width_scrub_commits_once() {
        let (mut app, id) = app_with_path();
        // A scrub: several live previews, then one commit on release.
        app.path_set_stroke_width(3.0);
        app.path_set_stroke_width(6.0);
        app.path_set_stroke_width(8.0);
        app.path_style_commit();
        let s = style(&app, id);
        assert!((s.stroke_style.width - 8.0).abs() < 1e-3);
        assert!(
            matches!(s.stroke, Paint::Solid(_)),
            "width implies an outline"
        );
        // A single undo returns to the original (no outline, default width).
        app.docs.documents[0].canvas.undo().expect("undo");
        assert!(matches!(style(&app, id).stroke, Paint::None));
    }

    #[test]
    fn custom_dash_and_offset_commit_as_one_undo_step() {
        let (mut app, id) = app_with_path();
        let mut values = [0.0; MAX_DASHES];
        values[..4].copy_from_slice(&[6.0, 2.0, 1.0, 2.0]);

        app.path_set_dash_values(values, 4);
        app.path_set_dash_offset(1.5);
        app.path_style_commit();

        let dash = style(&app, id).stroke_style.dash;
        assert_eq!(dash.as_slice(), &[6.0, 2.0, 1.0, 2.0]);
        assert!((dash.offset - 1.5).abs() < 1e-3);
        assert!(matches!(style(&app, id).stroke, Paint::Solid(_)));

        app.docs.documents[0].canvas.undo().expect("undo");
        assert!(style(&app, id).stroke_style.dash.is_solid());
        assert!(matches!(style(&app, id).stroke, Paint::None));
    }

    #[test]
    fn gradient_stop_add_remove_and_undo() {
        let (mut app, id) = app_with_path();
        app.path_set_fill_kind(1);

        app.path_add_gradient_stop();
        let Paint::Gradient(gradient) = style(&app, id).fill else {
            panic!("expected gradient");
        };
        assert_eq!(gradient.stop_count, 3);
        assert!((gradient.stops[1].offset - 0.5).abs() < 1e-3);

        app.docs.documents[0].canvas.undo().expect("undo add");
        let Paint::Gradient(gradient) = style(&app, id).fill else {
            panic!("expected gradient");
        };
        assert_eq!(gradient.stop_count, 2);

        app.path_add_gradient_stop();
        app.path_remove_gradient_stop(1);
        let Paint::Gradient(gradient) = style(&app, id).fill else {
            panic!("expected gradient");
        };
        assert_eq!(gradient.stop_count, 2);
        app.docs.documents[0].canvas.undo().expect("undo remove");
        let Paint::Gradient(gradient) = style(&app, id).fill else {
            panic!("expected gradient");
        };
        assert_eq!(gradient.stop_count, 3);
    }

    #[test]
    fn gradient_stop_color_and_location_are_one_undo_step() {
        let (mut app, id) = app_with_path();
        app.path_set_fill_kind(1);

        app.path_set_gradient_stop_offset(1, 0.75);
        app.path_set_gradient_stop_color(1, [10, 20, 30, 255]);
        app.path_style_commit();

        let Paint::Gradient(gradient) = style(&app, id).fill else {
            panic!("expected gradient");
        };
        assert!((gradient.stops[1].offset - 0.75).abs() < 1e-3);
        assert_eq!(gradient.stops[1].color.to_rgba8(), [10, 20, 30, 255]);

        app.docs.documents[0].canvas.undo().expect("undo stop edit");
        let Paint::Gradient(gradient) = style(&app, id).fill else {
            panic!("expected gradient");
        };
        assert!((gradient.stops[1].offset - 1.0).abs() < 1e-3);
        assert_eq!(gradient.stops[1].color.to_rgba8(), [255, 255, 255, 255]);
    }

    #[test]
    fn cancelled_edit_records_nothing() {
        let (mut app, id) = app_with_path();
        let dirty_before = app.docs.documents[0].canvas.is_dirty();
        app.path_set_fill_color([10, 20, 30, 255]); // preview
                                                    // "Cancel": restore original, then commit sees no net change.
        app.path_set_fill_color([255, 0, 0, 255]);
        app.path_style_commit();
        assert_eq!(
            style(&app, id).fill,
            Paint::Solid(ColorValue::rgb(1.0, 0.0, 0.0))
        );
        assert_eq!(
            app.docs.documents[0].canvas.is_dirty(),
            dirty_before,
            "a no-net-change edit adds no history"
        );
    }

    #[test]
    fn palette_gesture_preserves_exact_cmyk_and_is_undoable() {
        let (mut app, id) = app_with_path();
        let pure_k = ColorValue::cmyk(0.0, 0.0, 0.0, 1.0);
        assert!(app.path_apply_palette_fill(pure_k));
        assert_eq!(style(&app, id).fill, Paint::Solid(pure_k));
        app.docs.documents[0].canvas.undo().expect("undo palette");
        assert_eq!(
            style(&app, id).fill,
            Paint::Solid(ColorValue::rgb(1.0, 0.0, 0.0))
        );
    }

    #[test]
    fn overprint_fill_and_outline_are_independent_undo_steps() {
        let (mut app, id) = app_with_path();
        app.path_apply_palette_outline(ColorValue::BLACK);
        app.path_set_fill_overprint(true);
        app.path_set_stroke_overprint(true);
        let current = style(&app, id);
        assert!(current.fill_overprint);
        assert!(current.stroke_overprint);

        app.docs.documents[0]
            .canvas
            .undo()
            .expect("undo outline OP");
        let current = style(&app, id);
        assert!(current.fill_overprint);
        assert!(!current.stroke_overprint);
        app.docs.documents[0].canvas.undo().expect("undo fill OP");
        assert!(!style(&app, id).fill_overprint);
    }

    #[test]
    fn removing_a_paint_also_clears_its_overprint_flag() {
        let (mut app, id) = app_with_path();
        app.path_set_fill_overprint(true);
        app.path_clear_palette_fill();
        let current = style(&app, id);
        assert_eq!(current.fill, Paint::None);
        assert!(!current.fill_overprint);
    }
}
