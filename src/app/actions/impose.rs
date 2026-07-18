// "Xếp ảnh in" — build a print-sheet document from the active photo.
//
// The active document's flattened composite is scaled once to the exact photo
// cell (aspect-preserving cover crop), rotated if the layout says so, and
// stamped N times into a NEW white 600dpi document: one layer per copy inside
// a group, so the user can move/delete/duplicate individual prints by hand.

use crate::app::render::CanvasEvent;
use crate::app::state::App;
use crate::core::canvas::Canvas;
use crate::core::imposition::{self, Paper, PhotoKind, SHEET_DPI};
use crate::core::layer::Layer;
use crate::core::tile::TileMap;

impl App {
    pub(crate) fn do_impose_sheet(&mut self, paper: Paper, kind: PhotoKind, gap: u32) {
        if self.modal_lock_active() {
            self.deny_modal_action();
            return;
        }
        if self.has_only_welcome_placeholder() {
            self.shell.status_msg = "Hãy mở ảnh đã crop trước".to_string();
            return;
        }

        let lay = imposition::layout(paper, kind, gap);
        if lay.placements.is_empty() {
            self.shell.status_msg = "Không xếp được — khe cắt quá lớn".to_string();
            return;
        }

        let src = &self.docs.documents[self.docs.active_doc_idx].canvas;
        let (src_w, src_h) = (src.width, src.height);
        if src_w == 0 || src_h == 0 {
            return;
        }
        let (cell_w, cell_h) = kind.cell_px();
        let src_ratio = src_w as f32 / src_h as f32;
        let cell_ratio = cell_w as f32 / cell_h as f32;
        let aspect_off = (src_ratio - cell_ratio).abs() / cell_ratio > 0.02;

        let rgba = src.flatten_for_export();
        let Some(img) = image::RgbaImage::from_raw(src_w, src_h, rgba) else {
            self.shell.status_msg = "Không đọc được ảnh nguồn".to_string();
            return;
        };
        // Scale once to the exact cell: aspect-preserving cover + centre crop,
        // so an off-size source is never distorted.
        let img = if (src_w, src_h) != (cell_w, cell_h) {
            let s = (cell_w as f32 / src_w as f32).max(cell_h as f32 / src_h as f32);
            let (fw, fh) = (
                ((src_w as f32 * s).round() as u32).max(cell_w),
                ((src_h as f32 * s).round() as u32).max(cell_h),
            );
            let scaled =
                image::imageops::resize(&img, fw, fh, image::imageops::FilterType::Lanczos3);
            image::imageops::crop_imm(
                &scaled,
                (fw - cell_w) / 2,
                (fh - cell_h) / 2,
                cell_w,
                cell_h,
            )
            .to_image()
        } else {
            img
        };
        let img = if lay.rotated {
            image::imageops::rotate90(&img)
        } else {
            img
        };
        let (img_w, img_h) = img.dimensions();
        let photo_tiles = TileMap::from_rgba(img.as_raw(), img_w, img_h);

        // New white sheet document at SHEET_DPI (same construction as do_new_tab).
        let (paper_w, paper_h) = paper.size_px();
        let id = crate::core::document::DocumentId(self.docs.next_doc_id);
        self.docs.next_doc_id += 1;
        let mut canvas = Canvas::new(paper_w, paper_h);
        canvas.metadata.resolution_ppi = SHEET_DPI;

        // Background (id 0) + one layer per copy, all inside a group. Children
        // sit contiguously below their header in the stack (same shape as
        // LayerStack group commands).
        let count = lay.placements.len() as u32;
        let group_id = count + 1;
        {
            let stack = &mut canvas.layer_stack;
            stack.layers[0].is_background = true;
            for (i, &(x, y)) in lay.placements.iter().enumerate() {
                let layer_id = i as u32 + 1;
                let mut layer = Layer::new(layer_id, &format!("Ảnh {}", i + 1), img_w, img_h);
                layer.tiles = photo_tiles.clone();
                layer.offset = (x as i32, y as i32);
                layer.parent_id = Some(group_id);
                stack.layers.push(layer);
            }
            let group_name = format!("Ảnh thẻ {}", kind.label());
            let mut group = Layer::new_group(group_id, &group_name, paper_w, paper_h);
            group.selected = true;
            let group_idx = stack.layers.len();
            stack.layers.push(group);
            stack.active_idx = group_idx;
            stack.set_next_id(group_id + 1);
        }
        canvas.pixels_stale = true;
        canvas.ensure_pixels();

        let mut doc = crate::core::document::Document::new(id, paper_w, paper_h);
        doc.canvas = canvas;
        doc.title = format!("Trang {} — {} tấm {}", paper.label(), count, kind.label());
        doc.path = None;

        self.docs.documents.push(doc);
        self.docs.active_doc_idx = self.docs.documents.len() - 1;
        self.touch_doc_mru();
        self.docs.current_file = None;
        self.shell.ui.show_welcome = false;
        if let Some(gpu) = &mut self.win.gpu {
            gpu.resize_canvas_texture(paper_w, paper_h);
            gpu.compositor.tile_atlas.clear();
            gpu.compositor.ping_initialized = false;
            gpu.compositor.last_result_is_ping = false;
        }
        self.fit_canvas_to_screen();
        self.push_canvas_uniforms();
        self.upload_full();
        self.upload_selection_mask();
        self.apply_canvas_event(CanvasEvent::LayerStructureChanged);

        self.shell.status_msg = if aspect_off {
            format!(
                "Đã xếp {count} tấm {} lên {} — LƯU Ý: ảnh nguồn lệch tỷ lệ {}, đã cắt giữa cho vừa ô",
                kind.label(),
                paper.label(),
                kind.label()
            )
        } else {
            format!(
                "Đã xếp {count} tấm {} lên trang {}",
                kind.label(),
                paper.label()
            )
        };
        if let Some(w) = &self.win.window {
            w.request_redraw();
        }
    }
}
