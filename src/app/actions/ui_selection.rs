//! apply_ui_actions handlers: selection modify ops, the Refine Selection
//! panel and clipboard/fill shortcuts. Split out of actions.rs (phase 2).

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::ui::UiActions;

fn consecutive_pdf_pages(selected_pages: &[usize], active_page: usize, count: usize) -> Vec<usize> {
    let start = selected_pages
        .iter()
        .position(|&page| page == active_page)
        .unwrap_or(0);
    let end = start.saturating_add(count).min(selected_pages.len());
    selected_pages[start..end].to_vec()
}

impl App {
    fn set_pdf_global_stamp_source_visible(
        &mut self,
        stamp: &crate::core::document::PdfGlobalStamp,
        visible: bool,
    ) {
        let (Some(source_page), Some(layer_id)) = (stamp.source_page, stamp.source_layer_id) else {
            return;
        };
        let doc = &mut self.docs.documents[self.docs.active_doc_idx];
        let Some(pdf) = doc.pdf_document.as_mut() else {
            return;
        };
        let canvas = if pdf.active_page == source_page {
            &mut doc.canvas
        } else if let Some(cached) = pdf.edited_pages.get_mut(&source_page) {
            &mut cached.canvas
        } else {
            return;
        };
        if let Some(layer) = canvas
            .layer_stack
            .layers
            .iter_mut()
            .find(|layer| layer.id == layer_id)
        {
            if layer.visible != visible {
                layer.visible = visible;
                canvas.layer_revision = canvas.layer_revision.wrapping_add(1);
            }
        }
    }

    pub(crate) fn open_pdf_batch_dialog(
        &mut self,
        operation: crate::ui::intent::PdfBatchOperation,
    ) {
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let Some(pdf) = doc.pdf_document.as_ref() else {
            self.shell.status_msg = "Tính năng này chỉ dùng cho tài liệu PDF nhiều trang".into();
            return;
        };
        let remaining = consecutive_pdf_pages(&pdf.selected_pages, pdf.active_page, usize::MAX)
            .len()
            .max(1);
        self.shell.ui.pdf_batch_operation = Some(operation);
        self.shell.ui.pdf_batch_page_count = remaining;
    }

    fn pdf_batch_target_pages(&self, count: usize) -> Vec<usize> {
        let doc = &self.docs.documents[self.docs.active_doc_idx];
        let Some(pdf) = doc.pdf_document.as_ref() else {
            return Vec::new();
        };
        consecutive_pdf_pages(&pdf.selected_pages, pdf.active_page, count)
    }

    pub(crate) fn clear_selection_on_pdf_pages(&mut self, target_pages: Vec<usize>) {
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
        let mut operation = match operation {
            Ok(operation) => operation,
            Err(message) => {
                self.shell.status_msg = message;
                return;
            }
        };
        operation.target_pages = target_pages;
        let page_count = {
            let doc = &mut self.docs.documents[doc_idx];
            let pdf = doc.pdf_document.as_mut().expect("checked above");
            let page_count = operation.target_pages.len();
            pdf.global_edits
                .push(crate::core::document::PdfGlobalEdit::Clear(operation));
            pdf.global_edits_redo.clear();
            doc.rebuild_pdf_global_overlay();
            page_count
        };
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        self.shell.status_msg =
            format!("Đã xóa vùng chọn tại cùng vị trí trên {page_count} trang PDF đã chọn");
    }

    pub(crate) fn undo_pdf_global_clear(&mut self) {
        let doc = &mut self.docs.documents[self.docs.active_doc_idx];
        let Some(pdf) = doc.pdf_document.as_mut() else {
            return;
        };
        let Some(operation) = pdf.global_edits.pop() else {
            return;
        };
        let source = match &operation {
            crate::core::document::PdfGlobalEdit::Stamp(stamp) => Some(stamp.clone()),
            crate::core::document::PdfGlobalEdit::Clear(_) => None,
        };
        pdf.global_edits_redo.push(operation);
        doc.rebuild_pdf_global_overlay();
        if let Some(stamp) = source {
            self.set_pdf_global_stamp_source_visible(&stamp, true);
        }
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        self.shell.status_msg = "Đã hoàn tác thao tác trên mọi trang PDF".into();
    }

    pub(crate) fn redo_pdf_global_clear(&mut self) {
        let doc = &mut self.docs.documents[self.docs.active_doc_idx];
        let Some(pdf) = doc.pdf_document.as_mut() else {
            return;
        };
        let Some(operation) = pdf.global_edits_redo.pop() else {
            return;
        };
        let source = match &operation {
            crate::core::document::PdfGlobalEdit::Stamp(stamp) => Some(stamp.clone()),
            crate::core::document::PdfGlobalEdit::Clear(_) => None,
        };
        pdf.global_edits.push(operation);
        doc.rebuild_pdf_global_overlay();
        if let Some(stamp) = source {
            self.set_pdf_global_stamp_source_visible(&stamp, false);
        }
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        self.shell.status_msg = "Đã làm lại thao tác trên mọi trang PDF".into();
    }

    pub(crate) fn stamp_active_layer_on_all_pdf_pages(
        &mut self,
        kind: crate::core::document::PdfGlobalStampKind,
        target_pages: Vec<usize>,
    ) {
        if self.edit.text_edit.is_some() {
            self.commit_text_edit();
        }
        let doc_idx = self.docs.active_doc_idx;
        let result = {
            let doc = &self.docs.documents[doc_idx];
            let Some(pdf) = doc.pdf_document.as_ref() else {
                self.shell.status_msg =
                    "Tính năng này chỉ dùng cho tài liệu PDF nhiều trang".into();
                return;
            };
            let layer = doc.canvas.layer_stack.active_layer();
            crate::core::document::PdfGlobalStamp::from_layer(
                layer,
                doc.canvas.width,
                doc.canvas.height,
                pdf.active_page,
                kind,
            )
        };
        let mut stamp = match result {
            Ok(stamp) => stamp,
            Err(message) => {
                self.shell.status_msg = message;
                return;
            }
        };
        stamp.target_pages = target_pages;
        let page_count = {
            let doc = &mut self.docs.documents[doc_idx];
            let pdf = doc.pdf_document.as_mut().expect("checked above");
            let source_layer_id = stamp.source_layer_id;
            let page_count = if stamp.target_pages.is_empty() {
                pdf.selected_pages.len()
            } else {
                stamp.target_pages.len()
            };
            pdf.global_edits
                .push(crate::core::document::PdfGlobalEdit::Stamp(stamp));
            pdf.global_edits_redo.clear();
            if let Some(layer_id) = source_layer_id {
                if let Some(layer) = doc
                    .canvas
                    .layer_stack
                    .layers
                    .iter_mut()
                    .find(|layer| layer.id == layer_id)
                {
                    layer.visible = false;
                    doc.canvas.layer_revision = doc.canvas.layer_revision.wrapping_add(1);
                }
            }
            doc.rebuild_pdf_global_overlay();
            page_count
        };
        self.apply_canvas_event(CanvasEvent::LayerPixelsChanged);
        let label = match kind {
            crate::core::document::PdfGlobalStampKind::Text => "văn bản",
            crate::core::document::PdfGlobalStampKind::Image => "hình ảnh",
        };
        self.shell.status_msg =
            format!("Đã thêm {label} tại cùng vị trí trên {page_count} trang PDF");
    }

    pub(super) fn handle_selection_refine_actions(&mut self, actions: &mut UiActions) {
        if let Some(count) = actions.sel.set_pdf_batch_page_count.take() {
            let max_pages = self.pdf_batch_target_pages(usize::MAX).len().max(1);
            self.shell.ui.pdf_batch_page_count = count.clamp(1, max_pages);
        }
        if std::mem::take(&mut actions.sel.cancel_pdf_batch) {
            self.shell.ui.pdf_batch_operation = None;
        }
        if std::mem::take(&mut actions.sel.apply_pdf_batch) {
            if let Some(operation) = self.shell.ui.pdf_batch_operation.take() {
                let target_pages = self.pdf_batch_target_pages(self.shell.ui.pdf_batch_page_count);
                match operation {
                    crate::ui::intent::PdfBatchOperation::Clear => {
                        self.clear_selection_on_pdf_pages(target_pages)
                    }
                    crate::ui::intent::PdfBatchOperation::Text => self
                        .stamp_active_layer_on_all_pdf_pages(
                            crate::core::document::PdfGlobalStampKind::Text,
                            target_pages,
                        ),
                }
            }
        }
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
            self.open_pdf_batch_dialog(crate::ui::intent::PdfBatchOperation::Clear);
        }
        if actions.sel.add_text_all_pdf_pages {
            self.open_pdf_batch_dialog(crate::ui::intent::PdfBatchOperation::Text);
        }
        if actions.sel.add_image_all_pdf_pages {
            self.stamp_active_layer_on_all_pdf_pages(
                crate::core::document::PdfGlobalStampKind::Image,
                Vec::new(),
            );
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

#[cfg(test)]
mod tests {
    use super::consecutive_pdf_pages;

    #[test]
    fn pdf_batch_range_starts_at_current_display_page_and_keeps_stable_ids() {
        let reordered_pages = [9, 3, 7, 4, 11];
        assert_eq!(consecutive_pdf_pages(&reordered_pages, 7, 2), vec![7, 4]);
        assert_eq!(
            consecutive_pdf_pages(&reordered_pages, 7, usize::MAX),
            vec![7, 4, 11]
        );
    }
}
