//! Shared 1-D snapping primitives.
//!
//! Used by guides (drag-from-ruler, ②), and designed to be reused by the layer
//! Move tool (③) and Free Transform (④): both ultimately snap a handful of
//! moving coordinates (an object's edges / center) to a set of fixed target
//! coordinates (canvas edges/center, guides, other layers' edges/centers) on
//! each axis independently. Keeping the math axis-agnostic (plain `f32`) lets the
//! same routine serve all three.

/// Default snap distance in screen pixels; divide by zoom for a canvas-space
/// threshold. Shared default for guides / move / transform.
pub const SNAP_THRESHOLD_PX: f32 = 6.0;

/// What a snap target represents — lets callers colour/prioritise the resulting
/// smart-guide overlay (e.g. red for a flush page-edge fit, magenta for object
/// alignment).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SnapKind {
    CanvasEdge,
    CanvasCenter,
    Guide,
    LayerEdge,
    LayerCenter,
}

impl SnapKind {
    /// Smart-guide colour intent: canvas edges read as a "flush to the page" fit
    /// (red); everything else is alignment (magenta/purple).
    pub fn is_canvas_edge(self) -> bool {
        matches!(self, SnapKind::CanvasEdge)
    }
}

/// Outcome of snapping a single value to the nearest target within threshold.
#[derive(Clone, Copy, Debug)]
pub struct Snap1D {
    /// The snapped value (equal to the matched target).
    pub value: f32,
    /// Correction applied: `value - original`.
    pub delta: f32,
    pub kind: SnapKind,
}

/// A smart-guide line drawn while snapping (layer Move ③, Free Transform ④).
#[derive(Clone, Copy, Debug)]
pub struct SnapLine {
    /// true = a vertical line (constant canvas X); false = horizontal (constant Y).
    pub vertical: bool,
    /// Canvas coordinate of the line.
    pub pos: f32,
    pub kind: SnapKind,
}

/// Snap `value` to the nearest of `targets` whose distance is `<= threshold`.
/// Returns `None` when no target is close enough. On ties the first-listed
/// target wins, so callers should order higher-priority targets first.
pub fn snap_1d(value: f32, targets: &[(f32, SnapKind)], threshold: f32) -> Option<Snap1D> {
    let mut best: Option<(f32, f32, SnapKind)> = None; // (target, dist, kind)
    for &(t, kind) in targets {
        let dist = (t - value).abs();
        if dist <= threshold && best.map_or(true, |(_, bd, _)| dist < bd) {
            best = Some((t, dist, kind));
        }
    }
    best.map(|(t, _, kind)| Snap1D {
        value: t,
        delta: t - value,
        kind,
    })
}

/// Snap whichever of several moving candidate coordinates (e.g. an object's
/// left / center / right edges) lands closest to a target. Returns the smallest
/// in-threshold correction along with the matched target `(adjustment, target_pos,
/// kind)`. Shared by the Move tool (③) and Free Transform (④).
pub fn best_snap(
    candidates: &[f32],
    targets: &[(f32, SnapKind)],
    threshold: f32,
) -> Option<(f32, f32, SnapKind)> {
    let mut best: Option<(f32, f32, f32, SnapKind)> = None; // (adj, dist, target, kind)
    for &c in candidates {
        if let Some(s) = snap_1d(c, targets, threshold) {
            let dist = s.delta.abs();
            if best.map_or(true, |(_, bd, _, _)| dist < bd) {
                best = Some((s.delta, dist, s.value, s.kind));
            }
        }
    }
    best.map(|(adj, _, target, kind)| (adj, target, kind))
}
