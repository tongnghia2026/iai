#![allow(dead_code)]
use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::canvas::Canvas;
use crate::core::units::Unit;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CropOverlay {
    None,
    RuleOfThirds,
    Grid,
    Diagonal,
    Triangle,
    GoldenRatio,
    GoldenSpiral,
}

impl CropOverlay {
    pub fn name(&self) -> &str {
        match self {
            CropOverlay::None => "None",
            CropOverlay::RuleOfThirds => "Rule of Thirds",
            CropOverlay::Grid => "Grid",
            CropOverlay::Diagonal => "Diagonal",
            CropOverlay::Triangle => "Triangle",
            CropOverlay::GoldenRatio => "Golden Ratio",
            CropOverlay::GoldenSpiral => "Golden Spiral",
        }
    }

    pub fn all() -> Vec<CropOverlay> {
        vec![
            CropOverlay::None,
            CropOverlay::RuleOfThirds,
            CropOverlay::Grid,
            CropOverlay::Diagonal,
            CropOverlay::Triangle,
            CropOverlay::GoldenRatio,
            CropOverlay::GoldenSpiral,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CropMode {
    Free,
    Ratio,
    FixedSize,
    Perspective,
}

impl CropMode {
    pub fn name(&self) -> &str {
        match self {
            CropMode::Free => "Free",
            CropMode::Ratio => "Ratio",
            CropMode::FixedSize => "Fixed Size",
            CropMode::Perspective => "Perspective",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CropPreset {
    pub name: String,
    pub width: f32,
    pub height: f32,
    pub unit: Unit,
    pub dpi: f32,
}

impl CropPreset {
    pub fn defaults() -> Vec<CropPreset> {
        vec![
            // ID-photo sizes as shops actually cut them ("3×4" = 2.8×3.8 cm),
            // at the 600dpi print pipeline used by "Xếp ảnh in".
            CropPreset {
                name: "Ảnh thẻ 3×4 (2.8×3.8cm 600dpi)".to_string(),
                width: 2.8,
                height: 3.8,
                unit: Unit::Centimeters,
                dpi: 600.0,
            },
            CropPreset {
                name: "Ảnh thẻ 4×6 (600dpi)".to_string(),
                width: 4.0,
                height: 6.0,
                unit: Unit::Centimeters,
                dpi: 600.0,
            },
            CropPreset {
                name: "Ảnh thẻ 2×3 (600dpi)".to_string(),
                width: 2.0,
                height: 3.0,
                unit: Unit::Centimeters,
                dpi: 600.0,
            },
            CropPreset {
                name: "Square 1:1".to_string(),
                width: 1.0,
                height: 1.0,
                unit: Unit::Pixels,
                dpi: 72.0,
            },
            CropPreset {
                name: "4:3".to_string(),
                width: 4.0,
                height: 3.0,
                unit: Unit::Pixels,
                dpi: 72.0,
            },
            CropPreset {
                name: "16:9".to_string(),
                width: 16.0,
                height: 9.0,
                unit: Unit::Pixels,
                dpi: 72.0,
            },
            CropPreset {
                name: "A4 Portrait 300dpi".to_string(),
                width: 2480.0,
                height: 3508.0,
                unit: Unit::Pixels,
                dpi: 300.0,
            },
            CropPreset {
                name: "A4 Landscape 300dpi".to_string(),
                width: 3508.0,
                height: 2480.0,
                unit: Unit::Pixels,
                dpi: 300.0,
            },
            CropPreset {
                name: "Instagram Square".to_string(),
                width: 1080.0,
                height: 1080.0,
                unit: Unit::Pixels,
                dpi: 72.0,
            },
            CropPreset {
                name: "Instagram Portrait".to_string(),
                width: 1080.0,
                height: 1350.0,
                unit: Unit::Pixels,
                dpi: 72.0,
            },
            CropPreset {
                name: "Twitter Header".to_string(),
                width: 1500.0,
                height: 500.0,
                unit: Unit::Pixels,
                dpi: 72.0,
            },
            CropPreset {
                name: "Facebook Cover".to_string(),
                width: 820.0,
                height: 312.0,
                unit: Unit::Pixels,
                dpi: 72.0,
            },
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CropHandle {
    None,
    TopLeft,
    Top,
    TopRight,
    Left,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
    Move,
    Rotate,
}

#[derive(Debug, Clone, Copy)]
struct CropBox {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

pub struct CropTool {
    pub mode: CropMode,
    pub overlay: CropOverlay,
    pub delete_cropped: bool,
    pub constrain_ratio: bool,
    pub ratio_w: f32,
    pub ratio_h: f32,
    pub fixed_w: f32,
    pub fixed_h: f32,
    pub unit: Unit,
    pub dpi: f32,
    /// True once the user has typed a resolution into the Crop options. A
    /// hand-entered DPI must survive tool/tab switches — only automatic
    /// defaults may be replaced by the document's metadata DPI.
    pub dpi_user_set: bool,
    pub presets: Vec<CropPreset>,

    pub is_dragging: bool,
    pub crop_x0: f32,
    pub crop_y0: f32,
    pub crop_x1: f32,
    pub crop_y1: f32,
    pub active_handle: CropHandle,
    pub drag_start_x: f32,
    pub drag_start_y: f32,
    drag_start_box: CropBox,
    pub committed: bool,
    /// True when on_press fired while has_selection() — distinguishes a plain
    /// click (no drag) from drawing a new box.
    pub press_had_selection: bool,

    pub rotation: f32,
    pub image_tx: f32,
    pub image_ty: f32,
    rotate_start_angle: f32,
    rotate_box_start: f32,
    drag_start_image_tx: f32,
    drag_start_image_ty: f32,
}

impl CropTool {
    pub fn new() -> Self {
        Self {
            mode: CropMode::Free,
            overlay: CropOverlay::RuleOfThirds,
            delete_cropped: true,
            constrain_ratio: false,
            ratio_w: 1.0,
            ratio_h: 1.0,
            fixed_w: 800.0,
            fixed_h: 600.0,
            unit: Unit::Pixels,
            dpi: 72.0,
            dpi_user_set: false,
            presets: CropPreset::defaults(),
            is_dragging: false,
            crop_x0: 0.0,
            crop_y0: 0.0,
            crop_x1: 0.0,
            crop_y1: 0.0,
            active_handle: CropHandle::None,
            drag_start_x: 0.0,
            drag_start_y: 0.0,
            drag_start_box: CropBox {
                x0: 0.0,
                y0: 0.0,
                x1: 0.0,
                y1: 0.0,
            },
            committed: false,
            press_had_selection: false,
            rotation: 0.0,
            image_tx: 0.0,
            image_ty: 0.0,
            rotate_start_angle: 0.0,
            rotate_box_start: 0.0,
            drag_start_image_tx: 0.0,
            drag_start_image_ty: 0.0,
        }
    }

    pub fn apply_preset(&mut self, preset: &CropPreset) {
        self.fixed_w = preset.width;
        self.fixed_h = preset.height;
        self.unit = preset.unit;
        self.dpi = preset.dpi;
        self.dpi_user_set = false;
        self.mode = CropMode::FixedSize;
    }

    /// A resolution typed by the user in the Crop options. Sticky: it is never
    /// replaced by the document's metadata DPI on re-activation.
    pub fn set_user_dpi(&mut self, dpi: f32) {
        self.dpi = dpi.clamp(1.0, 10000.0);
        self.dpi_user_set = true;
    }

    /// Refresh resolution from the active image only when Crop has no fixed
    /// output preset AND the user has not typed a resolution themselves. A
    /// fixed-size preset owns its DPI (for example an ID-photo preset at
    /// 600 ppi) and a hand-entered value is a deliberate choice, so leaving and
    /// returning to Crop (switching tools or document tabs) must not replace
    /// either with the document's commonly-defaulted 72 ppi metadata.
    pub fn sync_dpi_on_activate(&mut self, canvas_dpi: f32) {
        if self.mode != CropMode::FixedSize && !self.dpi_user_set {
            self.dpi = canvas_dpi.max(1.0);
        }
    }

    pub fn commit(&mut self, canvas: &mut Canvas, background: Option<[u8; 4]>) {
        let x0 = self.crop_x0.min(self.crop_x1);
        let y0 = self.crop_y0.min(self.crop_y1);
        let x1 = self.crop_x0.max(self.crop_x1);
        let y1 = self.crop_y0.max(self.crop_y1);
        let box_w = x1 - x0;
        let box_h = y1 - y0;
        if box_w < 1.0 || box_h < 1.0 {
            self.cancel();
            return;
        }

        let out_w: u32;
        let out_h: u32;

        if self.mode == CropMode::FixedSize {
            let (w_px, h_px) = self.fixed_size_pixels(canvas.width, canvas.height);
            out_w = w_px.round() as u32;
            out_h = h_px.round() as u32;
        } else {
            out_w = box_w.round() as u32;
            out_h = box_h.round() as u32;
        }

        if out_w == 0 || out_h == 0 {
            self.cancel();
            return;
        }

        let no_scale = (out_w as f32 - box_w).abs() < 0.5 && (out_h as f32 - box_h).abs() < 0.5;
        let identity_image = self.rotation.abs() < 0.001
            && self.image_tx.abs() < 0.001
            && self.image_ty.abs() < 0.001;
        let committed = if identity_image && no_scale {
            let ix0 = x0.round() as i32;
            let iy0 = y0.round() as i32;
            let ix1 = x1.round() as i32;
            let iy1 = y1.round() as i32;
            let cw = (ix1 - ix0).max(1) as u32;
            let ch = (iy1 - iy0).max(1) as u32;
            match background {
                Some(color) => {
                    canvas.crop_with_background(ix0, iy0, cw, ch, self.delete_cropped, color)
                }
                None => canvas.crop(ix0, iy0, cw, ch, self.delete_cropped),
            }
        } else {
            let cx = (x0 + x1) * 0.5;
            let cy = (y0 + y1) * 0.5;
            match background {
                Some(color) => canvas.crop_transformed_with_background(
                    cx,
                    cy,
                    box_w,
                    box_h,
                    out_w,
                    out_h,
                    self.image_tx,
                    self.image_ty,
                    self.rotation,
                    self.delete_cropped,
                    color,
                ),
                None => canvas.crop_transformed(
                    cx,
                    cy,
                    box_w,
                    box_h,
                    out_w,
                    out_h,
                    self.image_tx,
                    self.image_ty,
                    self.rotation,
                    self.delete_cropped,
                ),
            }
        };
        if !committed {
            return;
        }

        if self.mode == CropMode::FixedSize {
            canvas.metadata.resolution_ppi = self.dpi.max(1.0);
        }

        self.cancel();
    }

    /// Output canvas size commit() would produce for the current box, or None
    /// if the box is degenerate (commit cancels). Mirrors commit()'s logic so the
    /// app can check the memory limit before committing.
    pub fn prospective_output_size(&self, canvas_w: u32, canvas_h: u32) -> Option<(u32, u32)> {
        let x0 = self.crop_x0.min(self.crop_x1);
        let y0 = self.crop_y0.min(self.crop_y1);
        let x1 = self.crop_x0.max(self.crop_x1);
        let y1 = self.crop_y0.max(self.crop_y1);
        let box_w = x1 - x0;
        let box_h = y1 - y0;
        if box_w < 1.0 || box_h < 1.0 {
            return None;
        }
        let (out_w, out_h) = if self.mode == CropMode::FixedSize {
            let (w_px, h_px) = self.fixed_size_pixels(canvas_w, canvas_h);
            (w_px.round() as u32, h_px.round() as u32)
        } else {
            (box_w.round() as u32, box_h.round() as u32)
        };
        if out_w == 0 || out_h == 0 {
            return None;
        }
        Some((out_w, out_h))
    }

    pub fn init_bounds(&mut self, cw: u32, ch: u32) {
        let cw_f = cw as f32;
        let ch_f = ch as f32;

        let ratio = if self.mode == CropMode::FixedSize {
            self.fixed_aspect_ratio(cw, ch)
        } else if self.mode == CropMode::Ratio && self.ratio_h > 0.0 {
            Some(self.ratio_w / self.ratio_h)
        } else {
            None
        };

        if let Some(r) = ratio {
            let img_ratio = cw_f / ch_f;
            let (box_w, box_h) = if r > img_ratio {
                (cw_f, cw_f / r)
            } else {
                (ch_f * r, ch_f)
            };
            self.crop_x0 = (cw_f - box_w) * 0.5;
            self.crop_y0 = (ch_f - box_h) * 0.5;
            self.crop_x1 = self.crop_x0 + box_w;
            self.crop_y1 = self.crop_y0 + box_h;
            self.rotation = 0.0;
            self.image_tx = 0.0;
            self.image_ty = 0.0;
        } else {
            self.crop_x0 = 0.0;
            self.crop_y0 = 0.0;
            self.crop_x1 = cw_f;
            self.crop_y1 = ch_f;
            self.rotation = 0.0;
            self.image_tx = 0.0;
            self.image_ty = 0.0;
        }
    }

    /// Switch the W/H display unit.
    ///
    /// In `FixedSize` mode `fixed_w`/`fixed_h` store a *physical* size in the
    /// current unit, so the numbers are converted through pixels to keep the real
    /// size unchanged (e.g. 2480 px at 300 dpi → 21.00 cm) rather than keeping the
    /// raw number and merely relabelling the unit. The pixel selection is
    /// unchanged, so it is only re-synced to the new numbers. In Free/Ratio mode
    /// the crop is defined by the pixel selection and W/H is recomputed from it
    /// each frame, so only the unit label changes — never the selection.
    pub fn change_display_unit(&mut self, new_unit: Unit, cw: f32, ch: f32) {
        use crate::core::units::{from_pixels, to_pixels};
        if self.mode == CropMode::FixedSize {
            let w_px = to_pixels(self.fixed_w, self.unit, self.dpi, cw);
            let h_px = to_pixels(self.fixed_h, self.unit, self.dpi, ch);
            self.unit = new_unit;
            self.fixed_w = from_pixels(w_px, new_unit, self.dpi, cw);
            self.fixed_h = from_pixels(h_px, new_unit, self.dpi, ch);
            if self.has_selection() {
                self.init_bounds(cw as u32, ch as u32);
            }
        } else {
            self.unit = new_unit;
        }
    }

    /// Convert fixed_w/fixed_h (in current unit+dpi) to canvas pixels.
    pub fn fixed_size_pixels(&self, canvas_w: u32, canvas_h: u32) -> (f32, f32) {
        use crate::core::units::to_pixels;
        let w_px = to_pixels(self.fixed_w, self.unit, self.dpi, canvas_w as f32).max(1.0);
        let h_px = to_pixels(self.fixed_h, self.unit, self.dpi, canvas_h as f32).max(1.0);
        (w_px, h_px)
    }

    fn fixed_aspect_ratio(&self, canvas_w: u32, canvas_h: u32) -> Option<f32> {
        if self.fixed_w <= 0.0 || self.fixed_h <= 0.0 {
            return None;
        }
        let (w_px, h_px) = self.fixed_size_pixels(canvas_w, canvas_h);
        if w_px.is_finite() && h_px.is_finite() && h_px > 0.0 {
            Some(w_px / h_px)
        } else {
            None
        }
    }

    pub fn cancel(&mut self) {
        self.is_dragging = false;
        self.crop_x0 = 0.0;
        self.crop_y0 = 0.0;
        self.crop_x1 = 0.0;
        self.crop_y1 = 0.0;
        self.committed = false;
        self.rotation = 0.0;
        self.image_tx = 0.0;
        self.image_ty = 0.0;
        self.active_handle = CropHandle::None;
        self.press_had_selection = false;
    }

    pub fn has_selection(&self) -> bool {
        (self.crop_x1 - self.crop_x0).abs() > 1.0 && (self.crop_y1 - self.crop_y0).abs() > 1.0
    }

    /// Detect which handle (if any) is under the pointer at `(x, y)` in canvas space.
    ///
    /// Hit-testing is done in *un-rotated* box space: the pointer is
    /// inverse-rotated around the box center before the axis-aligned checks,
    /// so handles track the visual corners correctly regardless of rotation.
    /// zoom: current view zoom (screen_px / canvas_px). Hit radius is 9 screen-px.
    pub fn detect_handle(&self, x: f32, y: f32, zoom: f32) -> CropHandle {
        let x0 = self.crop_x0.min(self.crop_x1);
        let y0 = self.crop_y0.min(self.crop_y1);
        let x1 = self.crop_x0.max(self.crop_x1);
        let y1 = self.crop_y0.max(self.crop_y1);

        let mx = (x0 + x1) / 2.0;
        let my = (y0 + y1) / 2.0;
        let r = (9.0 / zoom.max(0.01)).clamp(4.0, 24.0);

        let near = |a: f32, b: f32| (a - b).abs() < r;

        if near(x, x0) && near(y, y0) {
            return CropHandle::TopLeft;
        }
        if near(x, x1) && near(y, y0) {
            return CropHandle::TopRight;
        }
        if near(x, x0) && near(y, y1) {
            return CropHandle::BottomLeft;
        }
        if near(x, x1) && near(y, y1) {
            return CropHandle::BottomRight;
        }
        if near(x, mx) && near(y, y0) {
            return CropHandle::Top;
        }
        if near(x, mx) && near(y, y1) {
            return CropHandle::Bottom;
        }
        if near(x, x0) && near(y, my) {
            return CropHandle::Left;
        }
        if near(x, x1) && near(y, my) {
            return CropHandle::Right;
        }
        if x > x0 && x < x1 && y > y0 && y < y1 {
            return CropHandle::Move;
        }
        CropHandle::Rotate
    }

    /// Box center in canvas space.
    pub fn box_center(&self) -> (f32, f32) {
        (
            (self.crop_x0 + self.crop_x1) * 0.5,
            (self.crop_y0 + self.crop_y1) * 0.5,
        )
    }

    fn drag_start_center(&self) -> (f32, f32) {
        (
            (self.drag_start_box.x0 + self.drag_start_box.x1) * 0.5,
            (self.drag_start_box.y0 + self.drag_start_box.y1) * 0.5,
        )
    }

    fn set_box_from_center(&mut self, center_x: f32, center_y: f32, width: f32, height: f32) {
        let half_w = width.abs() * 0.5;
        let half_h = height.abs() * 0.5;
        self.crop_x0 = center_x - half_w;
        self.crop_x1 = center_x + half_w;
        self.crop_y0 = center_y - half_h;
        self.crop_y1 = center_y + half_h;
    }

    fn resize_from_center(&mut self, pointer_x: f32, pointer_y: f32, cw: f32, ch: f32, snap: f32) {
        let (center_x, center_y) = self.drag_start_center();
        let mut handle_x = pointer_x;
        let mut handle_y = pointer_y;

        match self.active_handle {
            CropHandle::Left | CropHandle::TopLeft | CropHandle::BottomLeft => {
                if handle_x.abs() < snap {
                    handle_x = 0.0;
                }
                if (handle_x - cw).abs() < snap {
                    handle_x = cw;
                }
            }
            CropHandle::Right | CropHandle::TopRight | CropHandle::BottomRight => {
                if handle_x.abs() < snap {
                    handle_x = 0.0;
                }
                if (handle_x - cw).abs() < snap {
                    handle_x = cw;
                }
            }
            _ => {}
        }

        match self.active_handle {
            CropHandle::Top | CropHandle::TopLeft | CropHandle::TopRight => {
                if handle_y.abs() < snap {
                    handle_y = 0.0;
                }
                if (handle_y - ch).abs() < snap {
                    handle_y = ch;
                }
            }
            CropHandle::Bottom | CropHandle::BottomLeft | CropHandle::BottomRight => {
                if handle_y.abs() < snap {
                    handle_y = 0.0;
                }
                if (handle_y - ch).abs() < snap {
                    handle_y = ch;
                }
            }
            _ => {}
        }

        let start_w = (self.drag_start_box.x1 - self.drag_start_box.x0).abs();
        let start_h = (self.drag_start_box.y1 - self.drag_start_box.y0).abs();
        let mut width = start_w;
        let mut height = start_h;

        match self.active_handle {
            CropHandle::Left
            | CropHandle::Right
            | CropHandle::TopLeft
            | CropHandle::TopRight
            | CropHandle::BottomLeft
            | CropHandle::BottomRight => {
                width = (handle_x - center_x).abs() * 2.0;
            }
            _ => {}
        }

        match self.active_handle {
            CropHandle::Top
            | CropHandle::Bottom
            | CropHandle::TopLeft
            | CropHandle::TopRight
            | CropHandle::BottomLeft
            | CropHandle::BottomRight => {
                height = (handle_y - center_y).abs() * 2.0;
            }
            _ => {}
        }

        self.set_box_from_center(center_x, center_y, width, height);
    }
}

impl Tool for CropTool {
    fn id(&self) -> &'static str {
        "crop"
    }
    fn name(&self) -> &str {
        "Crop"
    }
    fn shortcut(&self) -> Option<char> {
        Some('C')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Crop
    }

    fn on_cancel(&mut self) {
        self.cancel();
    }
    fn on_confirm(&mut self, ctx: &mut ToolCtx) {
        // A PNG with alpha intentionally has no Background layer. Extending or
        // rotating its crop must therefore sample outside the source as
        // transparent instead of silently filling it with the toolbar BG color.
        let background = ctx
            .canvas()
            .layer_stack
            .layers
            .iter()
            .any(|layer| layer.is_background)
            .then_some(ctx.bg_color);
        self.commit(ctx.canvas_mut(), background);
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        self.drag_start_x = cx;
        self.drag_start_y = cy;
        self.drag_start_box = CropBox {
            x0: self.crop_x0,
            y0: self.crop_y0,
            x1: self.crop_x1,
            y1: self.crop_y1,
        };
        self.drag_start_image_tx = self.image_tx;
        self.drag_start_image_ty = self.image_ty;
        self.is_dragging = true;
        self.committed = false;
        self.press_had_selection = self.has_selection();

        if self.has_selection() {
            self.active_handle = self.detect_handle(cx, cy, ctx.zoom);

            if self.active_handle == CropHandle::Rotate {
                let (bcx, bcy) = self.box_center();
                self.rotate_start_angle = (cy - bcy).atan2(cx - bcx);
                self.rotate_box_start = self.rotation;
            } else if self.active_handle == CropHandle::Move {
            }
        } else {
            let sx = cx;
            let sy = cy;
            self.crop_x0 = sx;
            self.crop_y0 = sy;
            self.crop_x1 = sx;
            self.crop_y1 = sy;
            self.rotation = 0.0;
            self.image_tx = 0.0;
            self.image_ty = 0.0;
            self.drag_start_box = CropBox {
                x0: sx,
                y0: sy,
                x1: sx,
                y1: sy,
            };
            self.active_handle = CropHandle::BottomRight;
        }

        ToolResponse::redraw()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        let is_resize_handle = !matches!(
            self.active_handle,
            CropHandle::Move | CropHandle::Rotate | CropHandle::None
        );
        let resize_from_center = event.alt && is_resize_handle;

        match self.active_handle {
            CropHandle::Rotate => {
                let (bcx, bcy) = self.box_center();
                let angle = (cy - bcy).atan2(cx - bcx);
                self.rotation = self.rotate_box_start + (angle - self.rotate_start_angle);
                return ToolResponse::redraw();
            }
            CropHandle::Move => {
                let total_dx = cx - self.drag_start_x;
                let total_dy = cy - self.drag_start_y;
                self.image_tx = self.drag_start_image_tx + total_dx;
                self.image_ty = self.drag_start_image_ty + total_dy;
            }
            CropHandle::TopLeft
            | CropHandle::Top
            | CropHandle::TopRight
            | CropHandle::Left
            | CropHandle::Right
            | CropHandle::BottomLeft
            | CropHandle::Bottom
            | CropHandle::BottomRight
                if resize_from_center =>
            {
                let cw = ctx.canvas().width as f32;
                let ch = ctx.canvas().height as f32;
                let snap = (8.0 / ctx.zoom.max(0.01)).clamp(2.0, 40.0);
                self.resize_from_center(cx, cy, cw, ch, snap);
            }
            CropHandle::TopLeft => {
                self.crop_x0 = cx;
                self.crop_y0 = cy;
            }
            CropHandle::TopRight => {
                self.crop_x1 = cx;
                self.crop_y0 = cy;
            }
            CropHandle::BottomLeft => {
                self.crop_x0 = cx;
                self.crop_y1 = cy;
            }
            CropHandle::BottomRight => {
                self.crop_x1 = cx;
                self.crop_y1 = cy;
            }
            CropHandle::Top => {
                self.crop_y0 = cy;
            }
            CropHandle::Bottom => {
                self.crop_y1 = cy;
            }
            CropHandle::Left => {
                self.crop_x0 = cx;
            }
            CropHandle::Right => {
                self.crop_x1 = cx;
            }
            CropHandle::None => {}
        }

        if is_resize_handle {
            let cw = ctx.canvas().width as f32;
            let ch = ctx.canvas().height as f32;
            let snap = (8.0 / ctx.zoom.max(0.01)).clamp(2.0, 40.0);

            if !resize_from_center {
                match self.active_handle {
                    CropHandle::Left | CropHandle::TopLeft | CropHandle::BottomLeft => {
                        if self.crop_x0.abs() < snap {
                            self.crop_x0 = 0.0;
                        }
                        if (self.crop_x0 - cw).abs() < snap {
                            self.crop_x0 = cw;
                        }
                    }
                    CropHandle::Right | CropHandle::TopRight | CropHandle::BottomRight => {
                        if self.crop_x1.abs() < snap {
                            self.crop_x1 = 0.0;
                        }
                        if (self.crop_x1 - cw).abs() < snap {
                            self.crop_x1 = cw;
                        }
                    }
                    _ => {}
                }
                match self.active_handle {
                    CropHandle::Top | CropHandle::TopLeft | CropHandle::TopRight => {
                        if self.crop_y0.abs() < snap {
                            self.crop_y0 = 0.0;
                        }
                        if (self.crop_y0 - ch).abs() < snap {
                            self.crop_y0 = ch;
                        }
                    }
                    CropHandle::Bottom | CropHandle::BottomLeft | CropHandle::BottomRight => {
                        if self.crop_y1.abs() < snap {
                            self.crop_y1 = 0.0;
                        }
                        if (self.crop_y1 - ch).abs() < snap {
                            self.crop_y1 = ch;
                        }
                    }
                    _ => {}
                }
            }

            let has_ratio =
                self.mode == CropMode::Ratio && self.ratio_w > 0.0 && self.ratio_h > 0.0;
            let fixed_ratio = if self.mode == CropMode::FixedSize {
                self.fixed_aspect_ratio(ctx.canvas().width, ctx.canvas().height)
            } else {
                None
            };
            let has_fixed = fixed_ratio.is_some();
            let shift_ratio = self.mode == CropMode::Free && (event.shift || self.constrain_ratio);
            if has_ratio || has_fixed || shift_ratio {
                let ratio = if has_ratio {
                    self.ratio_h / self.ratio_w
                } else if let Some(fixed_ratio) = fixed_ratio {
                    1.0 / fixed_ratio
                } else {
                    let start_w = (self.drag_start_box.x1 - self.drag_start_box.x0).abs();
                    let start_h = (self.drag_start_box.y1 - self.drag_start_box.y0).abs();
                    if start_w > 1.0 && start_h > 1.0 {
                        start_h / start_w
                    } else {
                        1.0
                    }
                };

                let w = (self.crop_x1 - self.crop_x0).abs();
                let h = (self.crop_y1 - self.crop_y0).abs();

                let mut new_w = w;
                let mut new_h = w * ratio;

                match self.active_handle {
                    CropHandle::Top | CropHandle::Bottom => {
                        new_w = h / ratio;
                        new_h = h;
                    }
                    CropHandle::Left | CropHandle::Right => {
                        new_h = w * ratio;
                        new_w = w;
                    }
                    CropHandle::TopLeft
                    | CropHandle::TopRight
                    | CropHandle::BottomLeft
                    | CropHandle::BottomRight => {
                        new_h = w * ratio;
                    }
                    _ => {}
                }

                if resize_from_center {
                    let (center_x, center_y) = self.drag_start_center();
                    self.set_box_from_center(center_x, center_y, new_w, new_h);
                } else {
                    match self.active_handle {
                        CropHandle::Top | CropHandle::Bottom => {
                            let cx = (self.crop_x0 + self.crop_x1) * 0.5;
                            self.crop_x0 = cx - new_w * 0.5;
                            self.crop_x1 = self.crop_x0 + new_w;
                            if self.active_handle == CropHandle::Top {
                                self.crop_y0 = self.crop_y1 - new_h;
                            } else {
                                self.crop_y1 = self.crop_y0 + new_h;
                            }
                        }
                        CropHandle::Left | CropHandle::Right => {
                            let cy = (self.crop_y0 + self.crop_y1) * 0.5;
                            self.crop_y0 = cy - new_h * 0.5;
                            self.crop_y1 = self.crop_y0 + new_h;
                            if self.active_handle == CropHandle::Left {
                                self.crop_x0 = self.crop_x1 - new_w;
                            } else {
                                self.crop_x1 = self.crop_x0 + new_w;
                            }
                        }
                        CropHandle::TopLeft => {
                            self.crop_x0 = self.crop_x1 - new_w;
                            self.crop_y0 = self.crop_y1 - new_h;
                        }
                        CropHandle::TopRight => {
                            self.crop_x1 = self.crop_x0 + new_w;
                            self.crop_y0 = self.crop_y1 - new_h;
                        }
                        CropHandle::BottomLeft => {
                            self.crop_x0 = self.crop_x1 - new_w;
                            self.crop_y1 = self.crop_y0 + new_h;
                        }
                        CropHandle::BottomRight => {
                            self.crop_x1 = self.crop_x0 + new_w;
                            self.crop_y1 = self.crop_y0 + new_h;
                        }
                        _ => {}
                    }
                }
            }
        }

        ToolResponse::redraw()
    }

    fn on_release(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        self.is_dragging = false;

        if !self.press_had_selection {
            let dx = event.canvas_x - self.drag_start_x;
            let dy = event.canvas_y - self.drag_start_y;
            if (dx * dx + dy * dy).sqrt() < 5.0 {
                let (cw, ch) = {
                    let c = ctx.canvas();
                    (c.width, c.height)
                };
                let clicked_inside_canvas = event.canvas_x >= 0.0
                    && event.canvas_x <= cw as f32
                    && event.canvas_y >= 0.0
                    && event.canvas_y <= ch as f32;
                if clicked_inside_canvas {
                    self.init_bounds(cw, ch);
                } else {
                    self.cancel();
                }
            }
        }

        ToolResponse::redraw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::{Document, DocumentId};

    fn test_ctx(document: &mut Document) -> ToolCtx<'_> {
        ToolCtx::new(
            document,
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            1.0,
            0.0,
            0.0,
        )
    }

    fn alt_event(x: f32, y: f32) -> PointerEvent {
        let mut event = PointerEvent::new(x, y);
        event.alt = true;
        event
    }

    #[test]
    fn fixed_size_init_fits_visual_box_to_canvas() {
        let mut crop = CropTool::new();
        crop.mode = CropMode::FixedSize;
        crop.fixed_w = 2480.0;
        crop.fixed_h = 3508.0;
        crop.unit = Unit::Pixels;
        crop.init_bounds(400, 300);

        let w = (crop.crop_x1 - crop.crop_x0).abs();
        let h = (crop.crop_y1 - crop.crop_y0).abs();

        assert!(w <= 400.0);
        assert!(h <= 300.0);
        assert!((w / h - 2480.0 / 3508.0).abs() < 0.01);
    }

    #[test]
    fn expanding_crop_keeps_transparent_png_area_transparent() {
        let mut canvas = Canvas::from_rgba(
            vec![
                255, 0, 0, 255, 0, 0, 0, 0, // row 0
                0, 0, 0, 0, 0, 0, 0, 0, // row 1
            ],
            2,
            2,
        );
        assert!(!canvas.layer_stack.layers[0].is_background);

        let mut crop = CropTool::new();
        crop.crop_x0 = -1.0;
        crop.crop_y0 = -1.0;
        crop.crop_x1 = 3.0;
        crop.crop_y1 = 3.0;
        crop.commit(&mut canvas, None);

        assert_eq!((canvas.width, canvas.height), (4, 4));
        assert_eq!(canvas.layer_stack.layers.len(), 1);
        assert!(!canvas.layer_stack.layers[0].is_background);
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(0, 0).3,
            0,
            "new crop border must remain transparent"
        );
        assert_eq!(
            canvas.layer_stack.layers[0].tiles.get_pixel(1, 1),
            (255, 0, 0, 255),
            "original PNG content must remain aligned"
        );
    }

    #[test]
    fn fixed_size_percent_aspect_uses_canvas_dimensions() {
        let mut crop = CropTool::new();
        crop.mode = CropMode::FixedSize;
        crop.fixed_w = 200.0;
        crop.fixed_h = 50.0;
        crop.unit = Unit::Percent;
        crop.init_bounds(800, 400);

        let w = (crop.crop_x1 - crop.crop_x0).abs();
        let h = (crop.crop_y1 - crop.crop_y0).abs();

        assert!((w / h - 8.0).abs() < 0.01);
    }

    #[test]
    fn physical_fixed_size_values_stay_exact_when_dpi_changes() {
        let mut crop = CropTool::new();
        crop.mode = CropMode::FixedSize;
        crop.fixed_w = 10.0;
        crop.fixed_h = 15.0;
        crop.unit = Unit::Centimeters;
        crop.dpi = 72.0;
        let at_72 = crop.fixed_size_pixels(1000, 1000);
        assert!((at_72.0 - 283.46457).abs() < 0.001);
        assert!((at_72.1 - 425.19684).abs() < 0.001);

        crop.dpi = 300.0;
        let at_300 = crop.fixed_size_pixels(1000, 1000);
        assert!((at_300.0 - 1181.1024).abs() < 0.001);
        assert!((at_300.1 - 1771.6536).abs() < 0.001);
        assert_eq!((crop.fixed_w, crop.fixed_h), (10.0, 15.0));
    }

    #[test]
    fn switching_display_unit_converts_fixed_size_to_new_unit() {
        // A4 at 300 ppi as a pixel-unit preset: 2480 × 3508 px.
        let mut crop = CropTool::new();
        crop.mode = CropMode::FixedSize;
        crop.unit = Unit::Pixels;
        crop.dpi = 300.0;
        crop.fixed_w = 2480.0;
        crop.fixed_h = 3508.0;

        // Switching to cm must convert the real size, not keep the raw number.
        crop.change_display_unit(Unit::Centimeters, 4000.0, 4000.0);
        assert_eq!(crop.unit, Unit::Centimeters);
        assert!((crop.fixed_w - 21.0).abs() < 0.05, "w = {}", crop.fixed_w);
        assert!((crop.fixed_h - 29.7).abs() < 0.1, "h = {}", crop.fixed_h);

        // Physical size is preserved on the round-trip back to pixels.
        crop.change_display_unit(Unit::Pixels, 4000.0, 4000.0);
        assert!((crop.fixed_w - 2480.0).abs() < 1.0, "w = {}", crop.fixed_w);
        assert!((crop.fixed_h - 3508.0).abs() < 1.0, "h = {}", crop.fixed_h);
    }

    #[test]
    fn fixed_size_preset_keeps_dpi_when_crop_tool_is_reselected() {
        let mut crop = CropTool::new();
        let preset = CropPreset {
            name: "3x4 600dpi".to_string(),
            width: 2.8,
            height: 3.8,
            unit: Unit::Centimeters,
            dpi: 600.0,
        };
        crop.apply_preset(&preset);

        crop.sync_dpi_on_activate(72.0);

        assert_eq!(crop.dpi, 600.0);
        assert_eq!(crop.mode, CropMode::FixedSize);
    }

    #[test]
    fn free_crop_adopts_active_document_dpi() {
        let mut crop = CropTool::new();
        crop.dpi = 600.0;

        crop.sync_dpi_on_activate(300.0);

        assert_eq!(crop.dpi, 300.0);
    }

    #[test]
    fn user_typed_dpi_survives_reactivation() {
        let mut crop = CropTool::new();
        crop.set_user_dpi(300.0);

        // Switching tools or document tabs re-syncs from the document's
        // metadata (commonly 72 ppi); a hand-typed resolution must hold.
        crop.sync_dpi_on_activate(72.0);

        assert_eq!(crop.dpi, 300.0);
    }

    #[test]
    fn preset_dpi_then_free_mode_readopts_document_dpi() {
        let mut crop = CropTool::new();
        let preset = CropPreset {
            name: "ID".to_string(),
            width: 3.0,
            height: 4.0,
            unit: Unit::Centimeters,
            dpi: 600.0,
        };
        crop.apply_preset(&preset);
        crop.mode = CropMode::Free;

        crop.sync_dpi_on_activate(300.0);

        assert_eq!(crop.dpi, 300.0);
    }

    #[test]
    fn prospective_size_none_for_degenerate_box() {
        let mut crop = CropTool::new();
        crop.crop_x0 = 10.0;
        crop.crop_y0 = 10.0;
        crop.crop_x1 = 10.0;
        crop.crop_y1 = 10.0;
        assert_eq!(crop.prospective_output_size(400, 300), None);
    }

    #[test]
    fn prospective_size_reports_fixed_size_output() {
        let mut crop = CropTool::new();
        crop.mode = CropMode::FixedSize;
        crop.unit = Unit::Pixels;
        crop.fixed_w = 6000.0;
        crop.fixed_h = 5000.0;
        crop.crop_x0 = 0.0;
        crop.crop_y0 = 0.0;
        crop.crop_x1 = 100.0;
        crop.crop_y1 = 100.0;
        let (ow, oh) = crop.prospective_output_size(400, 300).unwrap();
        assert_eq!((ow, oh), (6000, 5000));
        assert!(!crate::core::canvas::Canvas::fits_flat_buffer(ow, oh));
    }

    #[test]
    fn alt_drag_corner_resizes_crop_from_center() {
        let mut crop = CropTool::new();
        crop.crop_x0 = 100.0;
        crop.crop_y0 = 100.0;
        crop.crop_x1 = 300.0;
        crop.crop_y1 = 300.0;

        let mut document = Document::new(DocumentId(1), 500, 500);
        let press = PointerEvent::new(300.0, 300.0);
        {
            let mut ctx = test_ctx(&mut document);
            crop.on_press(press, &mut ctx);
            crop.on_drag(alt_event(350.0, 360.0), &press, &mut ctx);
        }

        assert_eq!(crop.active_handle, CropHandle::BottomRight);
        assert!(((crop.crop_x0 + crop.crop_x1) * 0.5 - 200.0).abs() < 0.01);
        assert!(((crop.crop_y0 + crop.crop_y1) * 0.5 - 200.0).abs() < 0.01);
        assert!((crop.crop_x0 - 50.0).abs() < 0.01);
        assert!((crop.crop_x1 - 350.0).abs() < 0.01);
        assert!((crop.crop_y0 - 40.0).abs() < 0.01);
        assert!((crop.crop_y1 - 360.0).abs() < 0.01);
    }

    #[test]
    fn alt_drag_edge_resizes_only_that_axis_from_center() {
        let mut crop = CropTool::new();
        crop.crop_x0 = 100.0;
        crop.crop_y0 = 100.0;
        crop.crop_x1 = 300.0;
        crop.crop_y1 = 300.0;

        let mut document = Document::new(DocumentId(1), 500, 500);
        let press = PointerEvent::new(300.0, 200.0);
        {
            let mut ctx = test_ctx(&mut document);
            crop.on_press(press, &mut ctx);
            crop.on_drag(alt_event(350.0, 200.0), &press, &mut ctx);
        }

        assert_eq!(crop.active_handle, CropHandle::Right);
        assert!(((crop.crop_x0 + crop.crop_x1) * 0.5 - 200.0).abs() < 0.01);
        assert!(((crop.crop_y0 + crop.crop_y1) * 0.5 - 200.0).abs() < 0.01);
        assert!((crop.crop_x0 - 50.0).abs() < 0.01);
        assert!((crop.crop_x1 - 350.0).abs() < 0.01);
        assert!((crop.crop_y0 - 100.0).abs() < 0.01);
        assert!((crop.crop_y1 - 300.0).abs() < 0.01);
    }

    #[test]
    fn alt_drag_with_ratio_keeps_center_fixed() {
        let mut crop = CropTool::new();
        crop.mode = CropMode::Ratio;
        crop.ratio_w = 1.0;
        crop.ratio_h = 1.0;
        crop.crop_x0 = 100.0;
        crop.crop_y0 = 100.0;
        crop.crop_x1 = 300.0;
        crop.crop_y1 = 200.0;

        let mut document = Document::new(DocumentId(1), 500, 500);
        let press = PointerEvent::new(300.0, 200.0);
        {
            let mut ctx = test_ctx(&mut document);
            crop.on_press(press, &mut ctx);
            crop.on_drag(alt_event(350.0, 260.0), &press, &mut ctx);
        }

        assert_eq!(crop.active_handle, CropHandle::BottomRight);
        assert!(((crop.crop_x0 + crop.crop_x1) * 0.5 - 200.0).abs() < 0.01);
        assert!(((crop.crop_y0 + crop.crop_y1) * 0.5 - 150.0).abs() < 0.01);
        assert!(
            ((crop.crop_x1 - crop.crop_x0).abs() - (crop.crop_y1 - crop.crop_y0).abs()).abs()
                < 0.01
        );
    }

    #[test]
    fn dragging_inside_moves_image_behind_fixed_crop_box() {
        let mut crop = CropTool::new();
        crop.crop_x0 = 100.0;
        crop.crop_y0 = 100.0;
        crop.crop_x1 = 300.0;
        crop.crop_y1 = 300.0;

        let mut document = Document::new(DocumentId(1), 500, 500);
        let press = PointerEvent::new(200.0, 200.0);
        {
            let mut ctx = test_ctx(&mut document);
            crop.on_press(press, &mut ctx);
            crop.on_drag(PointerEvent::new(230.0, 240.0), &press, &mut ctx);
        }

        assert_eq!(crop.active_handle, CropHandle::Move);
        assert!((crop.crop_x0 - 100.0).abs() < 0.01);
        assert!((crop.crop_y0 - 100.0).abs() < 0.01);
        assert!((crop.crop_x1 - 300.0).abs() < 0.01);
        assert!((crop.crop_y1 - 300.0).abs() < 0.01);
        assert!((crop.image_tx - 30.0).abs() < 0.01);
        assert!((crop.image_ty - 40.0).abs() < 0.01);
    }

    #[test]
    fn new_crop_drag_can_start_outside_canvas_bounds() {
        let mut crop = CropTool::new();
        let mut document = Document::new(DocumentId(1), 500, 500);
        let press = PointerEvent::new(-40.0, -25.0);
        {
            let mut ctx = test_ctx(&mut document);
            crop.on_press(press, &mut ctx);
            crop.on_drag(PointerEvent::new(250.0, 260.0), &press, &mut ctx);
        }

        assert_eq!(crop.active_handle, CropHandle::BottomRight);
        assert!((crop.crop_x0 + 40.0).abs() < 0.01);
        assert!((crop.crop_y0 + 25.0).abs() < 0.01);
        assert!((crop.crop_x1 - 250.0).abs() < 0.01);
        assert!((crop.crop_y1 - 260.0).abs() < 0.01);
    }

    #[test]
    fn click_inside_canvas_initializes_crop_box() {
        let mut crop = CropTool::new();
        let mut document = Document::new(DocumentId(1), 500, 400);
        let click = PointerEvent::new(250.0, 200.0);

        {
            let mut ctx = test_ctx(&mut document);
            crop.on_press(click, &mut ctx);
            crop.on_release(click, &mut ctx);
        }

        assert!(crop.has_selection());
        assert_eq!(
            (crop.crop_x0, crop.crop_y0, crop.crop_x1, crop.crop_y1),
            (0.0, 0.0, 500.0, 400.0)
        );
    }

    #[test]
    fn click_outside_canvas_keeps_crop_idle() {
        let mut crop = CropTool::new();
        let mut document = Document::new(DocumentId(1), 500, 400);
        let click = PointerEvent::new(-20.0, 200.0);

        {
            let mut ctx = test_ctx(&mut document);
            crop.on_press(click, &mut ctx);
            crop.on_release(click, &mut ctx);
        }

        assert!(!crop.has_selection());
        assert_eq!(crop.active_handle, CropHandle::None);
    }

    #[test]
    fn resize_handle_can_extend_crop_outside_canvas_bounds() {
        let mut crop = CropTool::new();
        crop.crop_x0 = 100.0;
        crop.crop_y0 = 100.0;
        crop.crop_x1 = 300.0;
        crop.crop_y1 = 300.0;

        let mut document = Document::new(DocumentId(1), 500, 500);
        let press = PointerEvent::new(300.0, 300.0);
        {
            let mut ctx = test_ctx(&mut document);
            crop.on_press(press, &mut ctx);
            crop.on_drag(PointerEvent::new(560.0, 540.0), &press, &mut ctx);
        }

        assert_eq!(crop.active_handle, CropHandle::BottomRight);
        assert!((crop.crop_x0 - 100.0).abs() < 0.01);
        assert!((crop.crop_y0 - 100.0).abs() < 0.01);
        assert!((crop.crop_x1 - 560.0).abs() < 0.01);
        assert!((crop.crop_y1 - 540.0).abs() < 0.01);
    }

    #[test]
    fn rotating_crop_rotates_image_not_crop_box() {
        let mut crop = CropTool::new();
        crop.crop_x0 = 100.0;
        crop.crop_y0 = 100.0;
        crop.crop_x1 = 300.0;
        crop.crop_y1 = 300.0;

        let mut document = Document::new(DocumentId(1), 500, 500);
        let press = PointerEvent::new(350.0, 200.0);
        {
            let mut ctx = test_ctx(&mut document);
            crop.on_press(press, &mut ctx);
            crop.on_drag(PointerEvent::new(200.0, 350.0), &press, &mut ctx);
        }

        assert_eq!(crop.active_handle, CropHandle::Rotate);
        assert!((crop.crop_x0 - 100.0).abs() < 0.01);
        assert!((crop.crop_y0 - 100.0).abs() < 0.01);
        assert!((crop.crop_x1 - 300.0).abs() < 0.01);
        assert!((crop.crop_y1 - 300.0).abs() < 0.01);
        assert!((crop.rotation - std::f32::consts::FRAC_PI_2).abs() < 0.01);
    }
}
