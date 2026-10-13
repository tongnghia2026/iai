//! Patch — repair a region by dragging it over clean pixels (Source) or by
//! synthesising it from its surroundings (Smart). The interaction reuses
//! the Lasso/selection machinery: trace a freehand region (which becomes the active
//! selection), then drag that selection over a source area. The actual pixel edit is
//! NOT done here — it's deferred into a `PatchPending` that the App drains on
//! release (same contract as the Repair brush's content-aware path), keeping the
//! heavy Poisson/PatchMatch work out of the input frame.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::selection::{SelectionMode, SelectionSnapshot};
use crate::core::tile::TileMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchMode {
    /// Drag the region onto a clean area; that area's texture is Poisson-blended
    /// back into the region (seamless clone).
    Source,
    /// Synthesise the region from the surrounding content (PatchMatch); no drag
    /// needed — fires as soon as the region is drawn.
    Smart,
}

impl PatchMode {
    pub fn to_u8(self) -> u8 {
        match self {
            PatchMode::Source => 0,
            PatchMode::Smart => 1,
        }
    }
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => PatchMode::Smart,
            _ => PatchMode::Source,
        }
    }
}

/// A finished patch gesture waiting for the App to apply it on `mod.rs`/`input.rs`.
#[derive(Debug, Clone, Copy)]
pub enum PatchPending {
    /// Source mode: seamless-clone. The destination is the current (stationary)
    /// selection; the source is `dest + (dx, dy)` where `(dx, dy)` is the drag delta.
    Clone { dx: i32, dy: i32 },
    /// Smart: synthesise the current selection from its surroundings.
    Fill,
}

pub struct PatchTool {
    pub mode: PatchMode,

    // Freehand region drawing (mirrors LassoTool).
    points: Vec<(f32, f32)>,
    drawing: bool,
    base_snap: Option<SelectionSnapshot>,

    // Dragging an existing selection over a source. The selection stays put; a live
    // preview shows the source pixels in the destination. `base` is a clean snapshot
    // of the active layer taken when the drag begins.
    moving: bool,
    move_start: (f32, f32),
    delta: (i32, i32),
    base: Option<TileMap>,

    pending: Option<PatchPending>,
}

impl PatchTool {
    pub fn new() -> Self {
        Self {
            mode: PatchMode::Source,
            points: Vec::new(),
            drawing: false,
            base_snap: None,
            moving: false,
            move_start: (0.0, 0.0),
            delta: (0, 0),
            base: None,
            pending: None,
        }
    }

    /// Live freehand trace, for the App to draw as a preview path (reuses the
    /// Lasso preview rendering).
    pub fn preview_points(&self) -> &[(f32, f32)] {
        if self.drawing {
            &self.points
        } else {
            &[]
        }
    }

    /// Drain a finished patch gesture; the App applies it after `on_release`.
    pub fn take_pending(&mut self) -> Option<PatchPending> {
        self.pending.take()
    }
}

impl Default for PatchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for PatchTool {
    fn id(&self) -> &'static str {
        "patch"
    }
    fn name(&self) -> &str {
        "Patch"
    }
    fn shortcut(&self) -> Option<char> {
        Some('J')
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Patch
    }
    fn paints(&self) -> bool {
        true
    }

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        let canvas = ctx.canvas_mut();

        // Press inside an existing selection → drag it over a source (Source mode).
        // The selection stays put; we snapshot the layer so the drag can preview the
        // source pixels live without feeding back.
        if canvas.selection.active
            && cx >= 0.0
            && cy >= 0.0
            && canvas.selection.is_selected(cx as u32, cy as u32)
        {
            self.moving = true;
            self.move_start = (cx, cy);
            self.delta = (0, 0);
            self.base = if canvas.layer_stack.layers.is_empty() {
                None
            } else {
                canvas.layer_stack.normalize_active_idx();
                Some(canvas.active_layer().tiles.clone())
            };
            return ToolResponse::none();
        }

        // Otherwise start tracing a new region.
        self.points.clear();
        self.points.push((cx, cy));
        self.drawing = true;
        self.moving = false;
        self.base_snap = Some(canvas.snapshot_selection());
        ToolResponse::none()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if self.moving {
            let dx = (event.canvas_x - self.move_start.0).round() as i32;
            let dy = (event.canvas_y - self.move_start.1).round() as i32;
            if (dx, dy) != self.delta {
                self.delta = (dx, dy);
                if let Some(base) = &self.base {
                    ctx.canvas_mut().patch_preview(base, dx, dy);
                }
                return ToolResponse::repaint();
            }
            return ToolResponse::none();
        }

        if !self.drawing {
            return ToolResponse::none();
        }
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        if let Some(&(lx, ly)) = self.points.last() {
            if ((cx - lx).powi(2) + (cy - ly).powi(2)).sqrt() >= 2.0 {
                self.points.push((cx, cy));
            }
        } else {
            self.points.push((cx, cy));
        }
        ToolResponse::redraw()
    }

    fn on_release(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        if self.moving {
            self.moving = false;
            // Drop the live preview (restore the layer); the App then applies the real
            // seamless clone via `patch_clone`. The selection stayed at the destination,
            // so the drag delta is exactly the source offset.
            if let Some(base) = self.base.take() {
                ctx.canvas_mut().patch_preview_clear(&base);
            }
            if self.delta != (0, 0) && self.mode == PatchMode::Source {
                self.pending = Some(PatchPending::Clone {
                    dx: self.delta.0,
                    dy: self.delta.1,
                });
            }
            return ToolResponse::repaint();
        }

        if !self.drawing {
            return ToolResponse::none();
        }
        self.drawing = false;
        let (cx, cy) = (event.canvas_x, event.canvas_y);
        self.points.push((cx, cy));

        if self.points.len() >= 3 {
            let canvas = ctx.canvas_mut();
            if let Some(snap) = &self.base_snap {
                canvas.restore_selection_snapshot(snap);
            }
            canvas.select_polygon_mode(&self.points, SelectionMode::New, 0.0, true);
            // Smart fires immediately on the drawn region (no drag).
            if self.mode == PatchMode::Smart {
                self.pending = Some(PatchPending::Fill);
            }
        }
        self.points.clear();
        self.base_snap = None;
        ToolResponse::repaint()
    }

    fn on_cancel(&mut self) {
        self.points.clear();
        self.drawing = false;
        self.moving = false;
        self.base = None;
        self.base_snap = None;
        self.pending = None;
    }
}
