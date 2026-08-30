//! apply_ui_actions handlers: selection modify ops, the Refine Selection
//! panel and clipboard/fill shortcuts. Split out of actions.rs (phase 2).

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::ui::UiActions;

impl App {
    pub(crate) fn clear_selection_on_all_pdf_pages(&mut self) {
        let doc_idx = self.docs.active_doc_idx;
        let (canvas_width, canvas_height, active_is_background) = {
            let doc = &self.docs.documents[doc_idx];
            let canvas = &doc.canvas;
            (
                canvas.width,
                canvas.height,
                canvas
                    .layer_stack
                    .layers
                    .get(canvas.layer_stack.active_idx)
                    .is_some_and(|layer| layer.is_background),
            )
        };
        if self.docs.documents[doc_idx].pdf_document.is_none() {
            self.shell.status_msg = "Tính năng này chỉ dùng cho tài liệu PDF nhiều trang".into();
            return;
        }
        if !active_is_background {
            self.shell.status_msg =
                "Hãy chọn layer Background của trang PDF trước khi xóa mọi trang".into();
            return;
        }
        let operation = {
            let selection = &mut self.docs.documents[doc_idx].canvas.selection;
            crate::core::document::PdfGlobalClear::from_selection(
                selection,
                canvas_width,
                canvas_height,
                self.edit.bg_color,
            )
        };
        let operation = match operation {
            Ok(operation) => operation,
            Err(message) => {
                self.shell.status_msg = message;
                return;
            }
        };
        let page_count = {
            let doc = &mut self.docs.documents[doc_idx];
            let pdf = doc.pdf_document.as_mut().expect("checked above");
            pdf.global_clears.push(operation);
            pdf.global_clears_redo.clear();
            let page_count = pdf.page_count;
            doc.rebuild_pdf_global_overlay();
            page_count
        };
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        self.shell.status_msg =
            format!("Đã xóa vùng chọn tại cùng vị trí trên {page_count} trang PDF");
    }

    pub(crate) fn undo_pdf_global_clear(&mut self) {
        let doc = &mut self.docs.documents[self.docs.active_doc_idx];
        let Some(pdf) = doc.pdf_document.as_mut() else {
            return;
        };
        let Some(operation) = pdf.global_clears.pop() else {
            return;
        };
        pdf.global_clears_redo.push(operation);
        doc.rebuild_pdf_global_overlay();
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        self.shell.status_msg = "Đã hoàn tác lần xóa vùng trên mọi trang PDF".into();
    }

    pub(crate) fn redo_pdf_global_clear(&mut self) {
        let doc = &mut self.docs.documents[self.docs.active_doc_idx];
        let Some(pdf) = doc.pdf_document.as_mut() else {
            return;
        };
        let Some(operation) = pdf.global_clears_redo.pop() else {
            return;
        };
        pdf.global_clears.push(operation);
        doc.rebuild_pdf_global_overlay();
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        self.shell.status_msg = "Đã làm lại lần xóa vùng trên mọi trang PDF".into();
    }

    pub(super) fn handle_selection_refine_actions(&mut self, actions: &mut UiActions) {
        if let Some(mode) = actions.sel.set_selection_mode.take() {
            self.edit.selection_mode = mode;
        }
        if let Some(r) = actions.sel.selection_feather.take() {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .feather_selection(r);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        if let Some(px) = actions.sel.selection_grow.take() {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .grow_selection(px);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        if let Some(px) = actions.sel.selection_shrink.take() {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .shrink_selection(px);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        if let Some(r) = actions.sel.selection_smooth.take() {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .smooth_selection(r);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        if let Some(px) = actions.sel.selection_border.take() {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .border_selection(px);
            self.apply_canvas_event(CanvasEvent::SelectionChanged);
        }
        if let Some(kind) = actions.sel.open_modify_dialog.take() {
            self.shell.ui.show_modify_dialog = Some(kind);
        }
        if actions.sel.close_modify_dialog {
            self.shell.ui.show_modify_dialog = None;
        }

        if actions.sel.trigger_select_subject {
            self.do_select_subject();
        }

        if actions.sel.open_refine_panel {
            self.open_refine_panel();
        }
        if actions.sel.refine_cancel {
            self.cancel_refine_panel();
        }
        if actions.sel.refine_apply {
            self.commit_refine_panel();
        }

        if let Some(v) = actions.sel.set_refine_feather.take() {
            self.edit.refine_feather = v;
        }
        if let Some(v) = actions.sel.set_refine_smooth.take() {
            self.edit.refine_smooth = v;
        }
        if let Some(v) = actions.sel.set_refine_smart_radius.take() {
            self.edit.refine_smart_radius = v;
        }
        if let Some(v) = actions.sel.set_refine_shift_edge.take() {
            self.edit.refine_shift_edge = v;
        }
        if let Some(v) = actions.sel.set_refine_contrast.take() {
            self.edit.refine_contrast = v;
        }
        if let Some(v) = actions.sel.set_refine_decontaminate.take() {
            self.edit.refine_decontaminate = v;
        }
        if let Some(v) = actions.sel.set_refine_decontaminate_amount.take() {
            self.edit.refine_decontaminate_amount = v;
        }
        if actions.sel.trigger_refine_apply && self.edit.show_refine_panel {
            self.apply_refine_preview();
        }

        if let Some(v) = actions.sel.set_refine_brush_size.take() {
            self.edit.tools.refine_brush_mut().size = v;
        }
        if let Some(v) = actions.sel.set_refine_brush_hardness.take() {
            self.edit.tools.refine_brush_mut().hardness = v;
        }
        if let Some(v) = actions.sel.set_refine_brush_mode.take() {
            self.edit.tools.refine_brush_mut().mode = v;
        }
        if let Some(v) = actions.sel.set_refine_view_mode.take() {
            self.edit.refine_view_mode = v;
            self.edit.refine_overlay_tex = None;
            self.edit.refine_overlay_mask_rev = u64::MAX;
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
        if actions.sel.open_refine_color_dialog {
            self.shell.ui.show_refine_color_dialog = true;
            self.shell.ui.refine_color_dialog_color = self.edit.refine_overlay_color;
            self.shell.ui.refine_color_dialog_original = self.edit.refine_overlay_color;
            self.shell.ui.refine_color_dialog_center_next = true;
        }
        if actions.sel.refine_color_dialog_centered {
            self.shell.ui.refine_color_dialog_center_next = false;
        }
        if let Some(live_preview) = actions.sel.set_refine_color_dialog_live_preview.take() {
            self.shell.ui.refine_color_dialog_live_preview = live_preview;
            let color = if live_preview {
                self.shell.ui.refine_color_dialog_color
            } else {
                self.shell.ui.refine_color_dialog_original
            };
            self.set_refine_overlay_tint(color);
        }
        if let Some(color) = actions.sel.set_refine_color_dialog_color.take() {
            self.shell.ui.refine_color_dialog_color = color;
            if self.shell.ui.refine_color_dialog_live_preview {
                self.set_refine_overlay_tint(color);
            }
        }
        if actions.sel.refine_color_dialog_default {
            let color = [210, 30, 30, 190];
            self.shell.ui.refine_color_dialog_color = color;
            if self.shell.ui.refine_color_dialog_live_preview {
                self.set_refine_overlay_tint(color);
            }
        }
        if actions.sel.refine_color_dialog_ok {
            let color = self.shell.ui.refine_color_dialog_color;
            self.shell.ui.show_refine_color_dialog = false;
            self.shell.ui.refine_color_dialog_center_next = false;
            self.set_refine_overlay_tint(color);
        }
        if actions.sel.refine_color_dialog_cancel {
            let color = self.shell.ui.refine_color_dialog_original;
            self.shell.ui.show_refine_color_dialog = false;
            self.shell.ui.refine_color_dialog_color = color;
            self.shell.ui.refine_color_dialog_center_next = false;
            self.set_refine_overlay_tint(color);
        }
        if let Some(color) = actions.sel.set_refine_overlay_color.take() {
            self.set_refine_overlay_tint(color);
        }
        if let Some(v) = actions.sel.set_refine_output_mode.take() {
            self.edit.refine_output_mode = v;
        }

        if actions.doc.copy {
            self.do_copy();
        }
        if actions.doc.cut {
            self.do_cut();
        }
        if actions.doc.paste {
            self.do_paste();
        }
        if actions.doc.fill_foreground {
            self.edit.pending_fill = Some(self.edit.tools.brush().settings.color);
        }
        if actions.doc.fill_background {
            self.edit.pending_fill = Some(self.edit.bg_color);
        }
        if actions.sel.clear_selection_pixels {
            let has_selection = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .active;
            let active_is_background = {
                let canvas = &self.docs.documents[self.docs.active_doc_idx].canvas;
                canvas
                    .layer_stack
                    .layers
                    .get(canvas.layer_stack.active_idx)
                    .is_some_and(|layer| layer.is_background)
            };
            if has_selection && active_is_background {
                self.edit.pending_fill = Some(self.edit.bg_color);
            } else if has_selection
                && self.docs.documents[self.docs.active_doc_idx]
                    .canvas
                    .clear_selection()
            {
                self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
            }
        }
        if actions.sel.clear_selection_all_pdf_pages {
            self.clear_selection_on_all_pdf_pages();
        }
        if actions.sel.undo_pdf_global_clear {
            self.undo_pdf_global_clear();
        }
        if actions.sel.redo_pdf_global_clear {
            self.redo_pdf_global_clear();
        }
        if actions.sel.select_all {
            let (cw, ch) = {
                let d = &self.docs.documents[self.docs.active_doc_idx];
                (d.canvas.width, d.canvas.height)
            };
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .selection
                .select_all();
            self.apply_canvas_event(crate::app::render::CanvasEvent::SelectionChanged);
            self.shell.status_msg = format!("Selected all ({}×{})", cw, ch);
        }
        if actions.sel.deselect {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .deselect();
            self.upload_selection_mask();
            self.push_selection_uniforms();
        }
        if actions.sel.invert_selection {
            self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .invert_selection();
            self.upload_selection_mask();
            self.push_selection_uniforms();
        }
    }
}
