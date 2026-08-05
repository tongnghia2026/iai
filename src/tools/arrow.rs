//! One-drag Arrow / Connector tool.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::geometry::Point;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::color::ColorValue;
use crate::core::vector::from_shape::{elbow_connector_path, ConnectorRoute};
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::style::{ArrowHead, ArrowStyle, Paint, VectorStyle};

pub struct ArrowTool {
    pub width: f32,
    pub end_arrow: u8,
    pub route: u8,
    start: Option<Point>,
    end: Option<Point>,
    drawing: bool,
}

impl ArrowTool {
    pub fn new() -> Self {
        Self {
            width: 3.0,
            end_arrow: ArrowHead::Triangle.to_u8(),
            route: 0,
            start: None,
            end: None,
            drawing: false,
        }
    }

    pub fn is_drawing(&self) -> bool {
        self.drawing
    }

    /// Shift constrains an arrow to the dominant canvas axis, matching Move:
    /// horizontal when |dx| >= |dy|, otherwise vertical.
    fn constrained_end(&self, point: Point, shift: bool) -> Point {
        let Some(start) = self.start else {
            return point;
        };
        if !shift {
            return point;
        }
        let dx = point.x - start.x;
        let dy = point.y - start.y;
        if dx.abs() >= dy.abs() {
            Point::new(point.x, start.y)
        } else {
            Point::new(start.x, point.y)
        }
    }

    pub fn preview_path(&self) -> Option<crate::core::vector::path::PathData> {
        let (start, end) = (self.start?, self.end?);
        Some(elbow_connector_path(
            start.x,
            start.y,
            end.x,
            end.y,
            ConnectorRoute::from_u8(self.route),
        ))
    }

    pub fn take_arrow_object(&mut self, fg: [u8; 4]) -> Option<VectorObjectData> {
        let start = self.start.take()?;
        let end = self.end.take()?;
        self.drawing = false;
        if start.distance_to(end) < 1.0 {
            return None;
        }
        let mut style = VectorStyle::stroked(ColorValue::from_rgba8(fg), self.width.max(0.1));
        style.fill = Paint::None;
        style.stroke_style.end_arrow = ArrowStyle {
            kind: ArrowHead::from_u8(self.end_arrow),
            size: 3.0,
        };
        Some(VectorObjectData::new(
            elbow_connector_path(
                start.x,
                start.y,
                end.x,
                end.y,
                ConnectorRoute::from_u8(self.route),
            ),
            style,
            AffineTransform::IDENTITY,
        ))
    }

    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.drawing = false;
    }
}

impl Default for ArrowTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for ArrowTool {
    fn id(&self) -> &'static str {
        "arrow"
    }
    fn name(&self) -> &str {
        "Arrow / Connector"
    }
    fn icon(&self) -> &'static str {
        "arrow"
    }
    fn shortcut(&self) -> Option<char> {
        None
    }
    fn tool_id(&self) -> crate::tools::ToolId {
        crate::tools::ToolId::Arrow
    }

    fn on_press(&mut self, event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        let point = Point::new(event.canvas_x, event.canvas_y);
        self.start = Some(point);
        self.end = Some(point);
        self.drawing = true;
        ToolResponse::redraw()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        _ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if self.drawing {
            self.end =
                Some(self.constrained_end(Point::new(event.canvas_x, event.canvas_y), event.shift));
        }
        ToolResponse::redraw()
    }

    fn on_release(&mut self, event: PointerEvent, _ctx: &mut ToolCtx) -> ToolResponse {
        if self.drawing {
            self.end =
                Some(self.constrained_end(Point::new(event.canvas_x, event.canvas_y), event.shift));
            self.drawing = false;
        }
        ToolResponse::redraw()
    }

    fn on_cancel(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vector::style::ArrowHead;

    #[test]
    fn take_builds_open_arrow_path() {
        let mut tool = ArrowTool::new();
        tool.start = Some(Point::new(1.0, 2.0));
        tool.end = Some(Point::new(20.0, 12.0));
        tool.route = 3;
        let object = tool.take_arrow_object([10, 20, 30, 255]).unwrap();
        assert!(!object.path.contours[0].closed);
        assert_eq!(object.path.contours[0].nodes.len(), 4);
        assert_eq!(
            object.style.stroke_style.end_arrow.kind,
            ArrowHead::Triangle
        );
    }

    #[test]
    fn shift_constrains_arrow_to_dominant_axis() {
        let mut tool = ArrowTool::new();
        tool.start = Some(Point::new(10.0, 20.0));

        assert_eq!(
            tool.constrained_end(Point::new(70.0, 35.0), true),
            Point::new(70.0, 20.0)
        );
        assert_eq!(
            tool.constrained_end(Point::new(25.0, 90.0), true),
            Point::new(10.0, 90.0)
        );
        assert_eq!(
            tool.constrained_end(Point::new(70.0, 35.0), false),
            Point::new(70.0, 35.0)
        );
    }
}
