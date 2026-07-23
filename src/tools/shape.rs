// Shape tool — rubber-band a Rectangle, Ellipse, or Line, then create an
// editable vector Shape *layer* from it (see [`crate::core::shape::ShapeData`]
// and `app::shape_ops`). This tool object only holds the current draw settings
// (kind, fill/stroke colours, stroke width, corner radius) and the live
// rubber-band drag state used for the on-canvas preview; layer creation and
// handle editing happen at the App level, mirroring the Text tool.
//
// Modifiers while dragging: Shift constrains to a square / circle / 45°-snapped
// line; Alt anchors the drag at its centre.

use crate::extension::tool::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::tools::ToolId;

pub use crate::core::shape::ShapeKind;

pub struct ShapeTool {
    pub kind: ShapeKind,
    /// Fill the interior (Rectangle / Ellipse). Lines are always stroked.
    pub fill: bool,
    /// Interior colour (Rectangle / Ellipse).
    pub fill_color: [u8; 4],
    /// Outline / line thickness in canvas pixels. 0 = no outline.
    pub stroke_width: f32,
    /// Outline / line colour.
    pub stroke_color: [u8; 4],
    /// Rounded-rectangle corner radius in canvas pixels. 0 = sharp corners.
    pub corner_radius: f32,
    /// Polygon edge count / Star point count (3–100).
    pub sides: u32,
    /// Star inner-radius fraction of the outer radius (0–1).
    pub star_inner: f32,
    start_x: f32,
    start_y: f32,
    cur_x: f32,
    cur_y: f32,
    shift: bool,
    alt: bool,
    is_dragging: bool,
}

impl Default for ShapeTool {
    fn default() -> Self {
        Self {
            kind: ShapeKind::Rectangle,
            fill: true,
            fill_color: [0, 0, 0, 255],
            stroke_width: 2.0,
            stroke_color: [0, 0, 0, 255],
            corner_radius: 0.0,
            sides: 5,
            star_inner: 0.5,
            start_x: 0.0,
            start_y: 0.0,
            cur_x: 0.0,
            cur_y: 0.0,
            shift: false,
            alt: false,
            is_dragging: false,
        }
    }
}

impl ShapeTool {
    pub fn new() -> Self {
        Self::default()
    }

    /// End point after applying Shift constrain (square/circle/45°-line),
    /// relative to the drag start (before any Alt centring).
    fn effective_end(&self) -> (f32, f32) {
        if !self.shift {
            return (self.cur_x, self.cur_y);
        }
        let dx = self.cur_x - self.start_x;
        let dy = self.cur_y - self.start_y;
        match self.kind {
            ShapeKind::Line => {
                let len = (dx * dx + dy * dy).sqrt();
                if len < 0.001 {
                    return (self.cur_x, self.cur_y);
                }
                let ang = dy.atan2(dx);
                let step = std::f32::consts::FRAC_PI_4;
                let snapped = (ang / step).round() * step;
                (
                    self.start_x + len * snapped.cos(),
                    self.start_y + len * snapped.sin(),
                )
            }
            // Rectangle / Ellipse / Polygon / Star: Shift → square bounds.
            _ => {
                let m = dx.abs().max(dy.abs());
                (
                    self.start_x + m * dx.signum(),
                    self.start_y + m * dy.signum(),
                )
            }
        }
    }

    /// The two points defining the shape in canvas coords `(x0,y0,x1,y1)`, after
    /// Shift constrain and Alt (draw-from-centre).
    fn resolved_span(&self) -> (f32, f32, f32, f32) {
        let (ex, ey) = self.effective_end();
        if self.alt {
            (2.0 * self.start_x - ex, 2.0 * self.start_y - ey, ex, ey)
        } else {
            (self.start_x, self.start_y, ex, ey)
        }
    }

    /// Live rubber-band bounds `[x0,y0,x1,y1]`, or None when not dragging.
    pub fn preview_rect(&self) -> Option<[f32; 4]> {
        if !self.is_dragging {
            return None;
        }
        let (x0, y0, x1, y1) = self.resolved_span();
        Some([x0, y0, x1, y1])
    }

    pub fn preview_kind(&self) -> ShapeKind {
        self.kind
    }

    pub fn is_dragging(&self) -> bool {
        self.is_dragging
    }

    /// Finalise a drag at `(cx,cy)`: clears the drag state and returns the span
    /// if it is large enough to be a real shape (else None → a click, ignored).
    pub fn finish_span(
        &mut self,
        cx: f32,
        cy: f32,
        shift: bool,
        alt: bool,
    ) -> Option<(f32, f32, f32, f32)> {
        self.cur_x = cx;
        self.cur_y = cy;
        self.shift = shift;
        self.alt = alt;
        self.is_dragging = false;
        let (x0, y0, x1, y1) = self.resolved_span();
        if (x1 - x0).abs() < 0.5 && (y1 - y0).abs() < 0.5 {
            return None;
        }
        Some((x0, y0, x1, y1))
    }
}

impl Tool for ShapeTool {
    fn id(&self) -> &'static str {
        "shape"
    }
    fn name(&self) -> &str {
        "Shape"
    }
    fn shortcut(&self) -> Option<char> {
        Some('u')
    }
    fn tool_id(&self) -> ToolId {
        ToolId::Shape
    }
    fn paints(&self) -> bool {
        true
    }

    fn on_press(&mut self, event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        self.start_x = event.canvas_x;
        self.start_y = event.canvas_y;
        self.cur_x = event.canvas_x;
        self.cur_y = event.canvas_y;
        self.shift = event.shift;
        self.alt = event.alt;
        self.is_dragging = true;
        ToolResponse::redraw()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        _ctx: &mut ToolCtx,
    ) -> ToolResponse {
        self.cur_x = event.canvas_x;
        self.cur_y = event.canvas_y;
        self.shift = event.shift;
        self.alt = event.alt;
        ToolResponse::redraw()
    }

    // Release is handled at the App level (creates / edits the Shape layer);
    // this only clears the drag so a stray dispatch can't leave a preview.
    fn on_release(&mut self, _event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        self.is_dragging = false;
        ToolResponse::redraw()
    }

    fn on_cancel(&mut self) {
        self.is_dragging = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_makes_square() {
        let mut t = ShapeTool::new();
        t.kind = ShapeKind::Rectangle;
        t.start_x = 0.0;
        t.start_y = 0.0;
        t.cur_x = 100.0;
        t.cur_y = 40.0;
        t.shift = true;
        let (ex, ey) = t.effective_end();
        assert_eq!((ex - t.start_x).abs(), (ey - t.start_y).abs());
        assert_eq!(ex, 100.0);
        assert_eq!(ey, 100.0);
    }

    #[test]
    fn alt_draws_from_center() {
        let mut t = ShapeTool::new();
        t.start_x = 50.0;
        t.start_y = 50.0;
        t.cur_x = 70.0;
        t.cur_y = 60.0;
        t.alt = true;
        let (x0, y0, x1, y1) = t.resolved_span();
        assert_eq!((x0, y0), (30.0, 40.0));
        assert_eq!((x1, y1), (70.0, 60.0));
        assert_eq!((x0 + x1) * 0.5, 50.0);
        assert_eq!((y0 + y1) * 0.5, 50.0);
    }

    #[test]
    fn finish_span_rejects_tiny_click() {
        let mut t = ShapeTool::new();
        t.start_x = 10.0;
        t.start_y = 10.0;
        t.is_dragging = true;
        assert!(t.finish_span(10.1, 10.1, false, false).is_none());
        assert!(!t.is_dragging());
    }
}
