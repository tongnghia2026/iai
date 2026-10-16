//! Dynamic connector bindings (vector gap #8).
//!
//! A connector is an ordinary arrow-headed Path layer, but when either end is
//! dropped on a shape it *sticks* to it: the endpoint is remembered as a fraction
//! of that shape's opaque bounding box, so it follows the shape as it moves or
//! resizes. The visible path is DERIVED from these anchors plus the targets'
//! current positions by [`crate::core::canvas::Canvas::refresh_connectors`] — the
//! same "rebuild on the structural recomposite, fingerprint-gated, outside undo"
//! pattern the PowerClip clip mask uses. A connector with no anchors is a plain
//! static arrow, exactly as before.

/// One endpoint of a connector, stuck to a target layer at a fractional position
/// within that layer's opaque bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConnectorAnchor {
    /// The layer this end sticks to.
    pub layer_id: u32,
    /// Fraction across the target's bbox width, `[0,1]` (0 = left edge, 1 = right).
    pub fx: f32,
    /// Fraction down the target's bbox height, `[0,1]` (0 = top, 1 = bottom).
    pub fy: f32,
}

impl ConnectorAnchor {
    /// Resolve this anchor to a canvas-space point given the target's opaque
    /// bounding box `(x, y, w, h)`.
    pub fn resolve(&self, rect: (f32, f32, f32, f32)) -> (f32, f32) {
        (rect.0 + self.fx * rect.2, rect.1 + self.fy * rect.3)
    }
}

/// A connector Path layer's live attachment: which shapes its two ends stick to
/// (either may be free = `None`) and how it routes between them (a
/// [`crate::core::vector::from_shape::ConnectorRoute`] as `u8`). The visible path
/// is derived from these plus the targets' positions.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ConnectorBinding {
    pub start: Option<ConnectorAnchor>,
    pub end: Option<ConnectorAnchor>,
    pub route: u8,
}

impl ConnectorBinding {
    /// At least one end is stuck to a shape (so the connector is dynamic).
    pub fn is_attached(&self) -> bool {
        self.start.is_some() || self.end.is_some()
    }
}
