//! One-drag Arrow / Connector tool.

use super::{PointerEvent, Tool, ToolCtx, ToolResponse};
use crate::core::geometry::Point;
use crate::core::vector::affine::AffineTransform;
use crate::core::vector::color::ColorValue;
use crate::core::vector::from_shape::{elbow_connector_path, ConnectorRoute};
use crate::core::vector::object::VectorObjectData;
use crate::core::vector::style::{ArrowHead, ArrowStyle, Paint, VectorStyle};

/// Screen-pixel radius within which an endpoint snaps to existing vector
/// geometry (a corner/endpoint, or an outline). Divided by zoom at snap time.
const SNAP_PX: f32 = 9.0;

pub struct ArrowTool {
    pub width: f32,
    pub end_arrow: u8,
    pub route: u8,
    /// Master snap toggle, mirrored from `UiState.snap_enabled` (same wiring as
    /// the Move tool). When on, both endpoints snap to nearby vector objects so
    /// arrows / connectors join at their corners and edges.
    pub snap_enabled: bool,
    /// "Branch" (multi-arrow) mode: the first drag lays a straight trunk line,
    /// each later drag adds a sub-arrow onto the SAME object (see
    /// `App::commit_arrow`). Every drag is a straight segment here; the trunk and
    /// each branch get the chosen arrowhead at their far end.
    pub multi: bool,
    start: Option<Point>,
    end: Option<Point>,
    drawing: bool,
    /// Canvas-space point the last endpoint snapped to, for the on-canvas marker.
    /// `None` when the pointer isn't currently snapped.
    snap_marker: Option<Point>,
}

impl ArrowTool {
    pub fn new() -> Self {
        Self {
            width: 3.0,
            end_arrow: ArrowHead::Triangle.to_u8(),
            route: 0,
            snap_enabled: true,
            multi: false,
            start: None,
            end: None,
            drawing: false,
            snap_marker: None,
        }
    }

    pub fn is_drawing(&self) -> bool {
        self.drawing
    }

    /// Where the pointer is currently snapped (canvas space), for the UI marker.
    pub fn snap_marker(&self) -> Option<Point> {
        self.snap_marker
    }

    /// Resolve where an endpoint should land: an object snap (which records the
    /// marker and wins over Shift, matching the "object snap overrides ortho"
    /// convention of CAD tools), else the Shift-constrained cursor point.
    fn resolve_endpoint(&mut self, raw: Point, shift: bool, ctx: &ToolCtx) -> Point {
        if self.snap_enabled {
            if let Some(hit) = ctx.snap_vector_point(raw, None, SNAP_PX) {
                self.snap_marker = Some(hit.point);
                return hit.point;
            }
        }
        self.snap_marker = None;
        self.constrained_end(raw, shift)
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

    /// The connector route in force: branch mode is always a straight segment
    /// (trunk + each sub-arrow), otherwise the user's chosen route.
    fn effective_route(&self) -> ConnectorRoute {
        if self.multi {
            ConnectorRoute::from_u8(0)
        } else {
            ConnectorRoute::from_u8(self.route)
        }
    }

    pub fn preview_path(&self) -> Option<crate::core::vector::path::PathData> {
        let (start, end) = (self.start?, self.end?);
        Some(elbow_connector_path(
            start.x,
            start.y,
            end.x,
            end.y,
            self.effective_route(),
        ))
    }

    /// The outline/arrowhead style a committed arrow (or branch) is drawn with.
    /// Shared by the single-arrow object and the App's branch builder so both
    /// stay identical.
    pub fn make_style(&self, fg: [u8; 4]) -> VectorStyle {
        let mut style = VectorStyle::stroked(ColorValue::from_rgba8(fg), self.width.max(0.1));
        style.fill = Paint::None;
        style.stroke_style.end_arrow = ArrowStyle {
            kind: ArrowHead::from_u8(self.end_arrow),
            size: 3.0,
        };
        style
    }

    pub fn take_arrow_object(&mut self, fg: [u8; 4]) -> Option<VectorObjectData> {
        let route = self.effective_route();
        let start = self.start.take()?;
        let end = self.end.take()?;
        self.drawing = false;
        self.snap_marker = None;
        if start.distance_to(end) < 1.0 {
            return None;
        }
        Some(VectorObjectData::new(
            elbow_connector_path(start.x, start.y, end.x, end.y, route),
            self.make_style(fg),
            AffineTransform::IDENTITY,
        ))
    }

    /// Consume the in-progress straight segment `(start, end)` for branch mode,
    /// resetting the tool. `None` when there is no segment or it is too short.
    pub fn take_straight_segment(&mut self) -> Option<(Point, Point)> {
        let start = self.start.take()?;
        let end = self.end.take()?;
        self.drawing = false;
        self.snap_marker = None;
        if start.distance_to(end) < 1.0 {
            return None;
        }
        Some((start, end))
    }

    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.drawing = false;
        self.snap_marker = None;
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

    fn on_press(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        let raw = Point::new(event.canvas_x, event.canvas_y);
        // No `start` yet, so this only applies an object snap (Shift is a no-op).
        let point = self.resolve_endpoint(raw, event.shift, ctx);
        self.start = Some(point);
        self.end = Some(point);
        self.drawing = true;
        ToolResponse::redraw()
    }

    fn on_drag(
        &mut self,
        event: PointerEvent,
        _prev: &PointerEvent,
        ctx: &mut ToolCtx,
    ) -> ToolResponse {
        if self.drawing {
            let raw = Point::new(event.canvas_x, event.canvas_y);
            self.end = Some(self.resolve_endpoint(raw, event.shift, ctx));
        }
        ToolResponse::redraw()
    }

    fn on_release(&mut self, event: PointerEvent, ctx: &mut ToolCtx) -> ToolResponse {
        if self.drawing {
            let raw = Point::new(event.canvas_x, event.canvas_y);
            self.end = Some(self.resolve_endpoint(raw, event.shift, ctx));
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

    // --- Object snapping (connect to another vector) ---

    use crate::core::document::{Document, DocumentId};
    use crate::core::gateway::ChangeKind;
    use crate::core::vector::path::{Contour, FillRule, Node, PathData};

    /// A document whose canvas holds one horizontal line from (100,100)→(300,100).
    fn doc_with_line() -> Document {
        let mut document = Document::new(DocumentId(1), 500, 500);
        let path = PathData::new(
            vec![Contour::new(
                vec![
                    Node::sharp(Point::new(100.0, 100.0)),
                    Node::sharp(Point::new(300.0, 100.0)),
                ],
                false,
            )],
            FillRule::NonZero,
        );
        let object = VectorObjectData::new(
            path,
            VectorStyle::stroked(ColorValue::BLACK, 2.0),
            AffineTransform::IDENTITY,
        );
        document
            .canvas
            .execute(
                Box::new(crate::core::command_vector::CreatePathLayer::new(
                    object, "Line",
                )),
                ChangeKind::LayerStructure,
            )
            .unwrap();
        document
    }

    fn ctx(document: &mut Document) -> ToolCtx<'_> {
        // zoom 1.0 → the 9px snap radius is 9 canvas units.
        ToolCtx::new(
            document,
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            1.0,
            0.0,
            0.0,
        )
    }

    #[test]
    fn endpoint_snaps_to_an_existing_line_end() {
        let mut document = doc_with_line();
        let mut tool = ArrowTool::new();
        // Press far from the line: no snap, start stays put.
        tool.on_press(PointerEvent::new(20.0, 20.0), &mut ctx(&mut document));
        // Drag near the line's (300,100) endpoint: it snaps there.
        let mut drag = PointerEvent::new(303.0, 102.0);
        drag.shift = false;
        tool.on_drag(
            drag,
            &PointerEvent::new(20.0, 20.0),
            &mut ctx(&mut document),
        );
        assert_eq!(tool.end, Some(Point::new(300.0, 100.0)));
        assert_eq!(tool.snap_marker(), Some(Point::new(300.0, 100.0)));
    }

    #[test]
    fn snap_overrides_shift_constraint() {
        let mut document = doc_with_line();
        let mut tool = ArrowTool::new();
        tool.on_press(PointerEvent::new(300.0, 20.0), &mut ctx(&mut document));
        // Shift would normally lock to the vertical axis (x=300), but a snap target
        // is in range, so the endpoint lands on the corner instead.
        let mut drag = PointerEvent::new(303.0, 98.0);
        drag.shift = true;
        tool.on_drag(
            drag,
            &PointerEvent::new(300.0, 20.0),
            &mut ctx(&mut document),
        );
        assert_eq!(tool.end, Some(Point::new(300.0, 100.0)));
    }

    #[test]
    fn no_snap_when_disabled() {
        let mut document = doc_with_line();
        let mut tool = ArrowTool::new();
        tool.snap_enabled = false;
        tool.on_press(PointerEvent::new(20.0, 20.0), &mut ctx(&mut document));
        tool.on_drag(
            PointerEvent::new(303.0, 102.0),
            &PointerEvent::new(20.0, 20.0),
            &mut ctx(&mut document),
        );
        assert_eq!(tool.end, Some(Point::new(303.0, 102.0)));
        assert_eq!(tool.snap_marker(), None);
    }
}
