//! Shapes and paths that stay editable: their geometry and style are kept,
//! their pixels made from them.

use iai_doc::shape::{ShapeData, ShapeKind};
use iai_doc::vector::object::VectorObjectData;
use iai_doc::vector::path::{Contour, FillRule, Node, NodeKind, PathData};
use iai_doc::vector::style::{LineCap, LineJoin, VectorStyle};
use iai_geom::geometry::Point;
use iai_geom::vector::affine::AffineTransform;
use iai_history::command::LayerStructureCommand;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{Bounds, RectPx, not_enabled, parse_color};
use crate::{CmdError, Command, CommandMeta, Effect, EngineEvent, Plan, Session, Validate};

/// The fill and outline colours of a shape or path: at least one.
fn look(
    fill: &Option<String>,
    stroke: &Option<String>,
) -> Result<(Option<[u8; 4]>, Option<[u8; 4]>), CmdError> {
    let fill = fill
        .as_deref()
        .map(|c| parse_color("fill", c))
        .transpose()?;
    let stroke = stroke
        .as_deref()
        .map(|c| parse_color("stroke", c))
        .transpose()?;
    if fill.is_none() && stroke.is_none() {
        return Err(CmdError::bad_param(
            "fill",
            "a fill or a stroke colour (or both)",
        ));
    }
    Ok((fill, stroke))
}

fn two() -> f32 {
    2.0
}

/// The layer a shape or path became.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Drawn {
    pub doc: u32,
    pub layer: u32,
    /// Where its pixels lie on the page.
    pub bounds: Bounds,
}

fn drawn(s: &mut Session<'_>, doc: u32, idx: usize) -> Result<Drawn, CmdError> {
    let canvas = s.canvas(Some(doc))?;
    let layer = canvas
        .layer_stack
        .layers
        .get(idx)
        .ok_or_else(|| CmdError::internal("new layer"))?;
    let out = Drawn {
        doc,
        layer: layer.id,
        bounds: Bounds {
            x: layer.offset.0,
            y: layer.offset.1,
            w: layer.width,
            h: layer.height,
        },
    };
    s.emit(EngineEvent::Structure { doc });
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    Rectangle,
    Ellipse,
    /// From the rectangle's top-left corner to its bottom-right.
    Line,
    /// A regular polygon inscribed in the rectangle.
    Polygon,
    Star,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ShapeParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    pub shape: Shape,
    /// The box the shape fills, in document pixels.
    pub rect: RectPx,
    /// Fill colour, "#RRGGBB" or "#RRGGBBAA"; no fill when omitted.
    pub fill: Option<String>,
    /// Outline colour; no outline when omitted.
    pub stroke: Option<String>,
    /// Outline width in pixels.
    #[serde(default = "two")]
    #[schemars(range(min = 0.0, max = 1000.0))]
    pub stroke_width: f32,
    /// Rounded corners of a rectangle, in pixels.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 100000.0))]
    pub corner_radius: f32,
    /// Sides of a polygon, points of a star.
    #[serde(default = "five")]
    #[schemars(range(min = 3, max = 100))]
    pub sides: u32,
    /// A star's inner radius as a share of the outer, 0.05 to 0.95.
    #[serde(default = "half")]
    #[schemars(range(min = 0.05, max = 0.95))]
    pub star_inner: f32,
}

fn five() -> u32 {
    5
}

fn half() -> f32 {
    0.5
}

impl Validate for ShapeParams {
    fn validate(&self) -> Result<(), CmdError> {
        look(&self.fill, &self.stroke).map(|_| ())
    }
}

static SHAPE: CommandMeta = CommandMeta {
    id: "vector.add_shape",
    label_vi: "Vẽ hình",
    label_en: "Add Shape",
    doc: "Draw a rectangle, ellipse, line, polygon or star as a new layer that stays editable \
          (sharp at any zoom, in PDF too). Fill and/or outline colours as \"#RRGGBB\".",
    effects: &[Effect::Edit],
    featured: true,
};

pub struct AddShape;

impl Command for AddShape {
    type Params = ShapeParams;
    type Output = Drawn;
    fn meta(&self) -> &'static CommandMeta {
        &SHAPE
    }
    fn plan(&self, s: &mut Session<'_>, p: ShapeParams) -> Result<Plan<Drawn>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let (fill, stroke) = look(&p.fill, &p.stroke)?;
        let kind = match p.shape {
            Shape::Rectangle => ShapeKind::Rectangle,
            Shape::Ellipse => ShapeKind::Ellipse,
            Shape::Line => ShapeKind::Line,
            Shape::Polygon => ShapeKind::Polygon,
            Shape::Star => ShapeKind::Star,
        };
        let r = p.rect;
        let (x0, y0) = (r.x as f32, r.y as f32);
        let (x1, y1) = (x0 + r.w as f32, y0 + r.h as f32);
        let stroke_width = if stroke.is_some() {
            p.stroke_width
        } else {
            0.0
        };
        let (mut data, offset) = ShapeData::from_canvas_span(
            kind,
            x0,
            y0,
            x1,
            y1,
            p.corner_radius,
            fill.is_some(),
            fill.unwrap_or([0, 0, 0, 0]),
            stroke_width,
            stroke.unwrap_or([0, 0, 0, 0]),
        );
        data.sides = p.sides.clamp(3, 100);
        data.star_inner = p.star_inner.clamp(0.05, 0.95);
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = canvas
            .add_shape_layer(data, offset)
            .ok_or_else(|| not_enabled("the shape draws nothing", "Hình không vẽ ra gì"))?;
        canvas.flatten_full();
        drawn(s, doc, idx).map(Plan::Done)
    }
}

/// One run of a path: points joined by straight lines, or by one smooth curve
/// through them.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Run {
    /// `[x, y]` in document pixels.
    #[schemars(length(min = 2, max = 100000))]
    pub points: Vec<[f32; 2]>,
    /// Join the last point back to the first.
    #[serde(default)]
    pub closed: bool,
    /// A smooth curve through every point instead of straight lines.
    #[serde(default)]
    pub smooth: bool,
}

impl Run {
    fn contour(&self) -> Contour {
        let at = |&[x, y]: &[f32; 2]| Point::new(x, y);
        let nodes = self
            .points
            .iter()
            .enumerate()
            .map(|(i, point)| {
                let anchor = at(point);
                if !self.smooth {
                    return Node::sharp(anchor);
                }
                // Catmull-Rom: the tangent at a point runs from the one before
                // it to the one after; an open run's ends lean on themselves.
                let prev = match i.checked_sub(1) {
                    Some(j) => self.points.get(j),
                    None if self.closed => self.points.last(),
                    None => None,
                };
                let next = match self.points.get(i.saturating_add(1)) {
                    Some(p) => Some(p),
                    None if self.closed => self.points.first(),
                    None => None,
                };
                let (from, to) = (prev.map_or(anchor, at), next.map_or(anchor, at));
                let (dx, dy) = ((to.x - from.x) / 6.0, (to.y - from.y) / 6.0);
                Node {
                    anchor,
                    in_handle: prev.map(|_| Point::new(anchor.x - dx, anchor.y - dy)),
                    out_handle: next.map(|_| Point::new(anchor.x + dx, anchor.y + dy)),
                    kind: NodeKind::Smooth,
                }
            })
            .collect();
        Contour::new(nodes, self.closed)
    }
}

/// How an outline turns at a corner.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Join {
    /// Pointed; a sharp corner can reach out up to 4 widths.
    #[default]
    Miter,
    Round,
    /// Cut off flat.
    Bevel,
}

/// How an open outline ends.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    /// Flat, at the end point.
    #[default]
    Butt,
    Round,
    /// Flat, half a width past the end point.
    Square,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PathParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    #[schemars(length(min = 1, max = 1000))]
    pub runs: Vec<Run>,
    /// Fill colour, "#RRGGBB" or "#RRGGBBAA"; no fill when omitted.
    pub fill: Option<String>,
    /// Outline colour; no outline when omitted.
    pub stroke: Option<String>,
    /// Outline width in pixels.
    #[serde(default = "two")]
    #[schemars(range(min = 0.0, max = 1000.0))]
    pub stroke_width: f32,
    /// How the outline turns at a corner; `round` keeps sharp corners from
    /// spiking.
    #[serde(default)]
    pub join: Join,
    /// How an open outline ends.
    #[serde(default)]
    pub cap: Cap,
    /// Where runs overlap, holes alternate (`even_odd`) or the inside is
    /// filled whatever the direction (`non_zero`, default).
    #[serde(default)]
    pub even_odd: bool,
    #[schemars(length(min = 1, max = 200))]
    pub name: Option<String>,
}

impl Validate for PathParams {
    fn validate(&self) -> Result<(), CmdError> {
        look(&self.fill, &self.stroke)?;
        let finite = self
            .runs
            .iter()
            .flat_map(|r| r.points.iter())
            .all(|[x, y]| x.is_finite() && y.is_finite() && x.abs() < 1e6 && y.abs() < 1e6);
        if !finite {
            return Err(CmdError::bad_param("runs", "points within ±1000000 pixels"));
        }
        Ok(())
    }
}

static PATH: CommandMeta = CommandMeta {
    id: "vector.add_path",
    label_vi: "Vẽ đường",
    label_en: "Add Path",
    doc: "Draw a path (one or more runs of points, open or closed; straight segments, or a \
          smooth curve through the points with `smooth`) as a new editable vector layer, filled \
          and/or outlined, with miter / round / bevel corners.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct AddPath;

impl Command for AddPath {
    type Params = PathParams;
    type Output = Drawn;
    fn meta(&self) -> &'static CommandMeta {
        &PATH
    }
    fn plan(&self, s: &mut Session<'_>, p: PathParams) -> Result<Plan<Drawn>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let (fill, stroke) = look(&p.fill, &p.stroke)?;
        let contours = p.runs.iter().map(Run::contour).collect();
        let rule = if p.even_odd {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        };
        let path = PathData::new(contours, rule);
        path.validate()
            .map_err(|e| CmdError::bad_param("runs", &format!("a drawable path ({e})")))?;
        let stroke_width = if stroke.is_some() {
            p.stroke_width
        } else {
            0.0
        };
        let mut style = VectorStyle::from_shape_fields(
            fill.is_some(),
            fill.unwrap_or([0, 0, 0, 0]),
            stroke_width,
            stroke.unwrap_or([0, 0, 0, 0]),
        );
        style.stroke_style.join = match p.join {
            Join::Miter => LineJoin::Miter,
            Join::Round => LineJoin::Round,
            Join::Bevel => LineJoin::Bevel,
        };
        style.stroke_style.cap = match p.cap {
            Cap::Butt => LineCap::Butt,
            Cap::Round => LineCap::Round,
            Cap::Square => LineCap::Square,
        };
        let object = VectorObjectData::new(path, style, AffineTransform::IDENTITY);
        let canvas = s.canvas_mut(Some(doc))?;
        let (cw, ch) = (canvas.width, canvas.height);
        let mut cmd = LayerStructureCommand::capture_before("Path", &canvas.layer_stack, cw, ch);
        let idx = canvas.layer_stack.add_layer(cw, ch);
        let layer = canvas
            .layer_stack
            .layers
            .get_mut(idx)
            .ok_or_else(|| CmdError::internal("new layer"))?;
        iai_vector::command_vector::apply_object_to_layer(layer, object);
        layer.name = p.name.clone().unwrap_or_else(|| "Path".to_string());
        cmd.capture_after(&canvas.layer_stack, cw, ch);
        canvas.record(Box::new(cmd));
        canvas.layer_revision = canvas.layer_revision.saturating_add(1);
        canvas.flatten_full();
        drawn(s, doc, idx).map(Plan::Done)
    }
}

// ---- curves and shaping ---------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CurvesParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// A shape or text layer (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
}
impl Validate for CurvesParams {}

static CURVES: CommandMeta = CommandMeta {
    id: "vector.to_curves",
    label_vi: "Chuyển thành đường cong",
    label_en: "Convert to Curves",
    doc: "Turn a shape or a text layer into editable Bézier paths (Convert to Curves): a shape \
          in its place, a text as one path per colour, where the letters were. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct ToCurves;

impl Command for ToCurves {
    type Params = CurvesParams;
    type Output = super::layer::LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &CURVES
    }
    fn plan(
        &self,
        s: &mut Session<'_>,
        p: CurvesParams,
    ) -> Result<Plan<super::layer::LayerChanged>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let idx = crate::layer_index(canvas, p.layer)?;
        let said = match canvas.layer_stack.layers.get(idx).map(|l| &l.layer_type) {
            Some(iai_doc::layer::LayerType::Text(_)) => {
                iai_canvas::shaping::text_to_curves(canvas, idx)
            }
            Some(iai_doc::layer::LayerType::Vector(
                iai_doc::vector::object::VectorGeometry::Primitive(_),
            )) => iai_canvas::shaping::shape_to_curves(canvas, idx),
            _ => {
                return Err(not_enabled(
                    "only a shape or a text layer turns into curves",
                    "Chỉ layer hình hoặc chữ mới chuyển được thành đường cong",
                ));
            }
        }
        .map_err(|e| not_enabled(e.clone(), e))?;
        let id = {
            let stack = &canvas.layer_stack;
            stack.layers.get(stack.active_idx).map(|l| l.id)
        };
        s.emit(EngineEvent::Structure { doc });
        s.notice(said.clone(), said);
        super::layer::answer(s, doc, id).map(Plan::Done)
    }
}

/// The four shaping operations, in CorelDRAW's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShapingOp {
    /// One shape of them all (the sources go).
    Weld,
    /// The bottom one, less the ones above it (they stay).
    Trim,
    /// Their overlap, as a new shape on top (they stay).
    Intersect,
    /// Each loses what the ones above it hide.
    Simplify,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ShapingParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    pub op: ShapingOp,
    /// The vector layers (two or more); the ones picked in the Layers panel
    /// when omitted.
    #[schemars(length(min = 2, max = 1000))]
    pub layers: Option<Vec<u32>>,
}
impl Validate for ShapingParams {}

static SHAPING: CommandMeta = CommandMeta {
    id: "vector.shaping",
    label_vi: "Ghép / cắt hình",
    label_en: "Shaping",
    doc: "Weld, Trim, Intersect or Simplify two or more vector layers (Object ▸ Shaping), the \
          bottom one the target. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Shaping;

impl Command for Shaping {
    type Params = ShapingParams;
    type Output = super::layer::LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &SHAPING
    }
    fn plan(
        &self,
        s: &mut Session<'_>,
        p: ShapingParams,
    ) -> Result<Plan<super::layer::LayerChanged>, CmdError> {
        use iai_vector::vector::boolean::BooleanOp;
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if let Some(ids) = &p.layers {
            let rows: Vec<usize> = ids
                .iter()
                .map(|id| crate::layer_index(canvas, Some(*id)))
                .collect::<Result<_, _>>()?;
            for (i, l) in canvas.layer_stack.layers.iter_mut().enumerate() {
                l.selected = rows.contains(&i);
            }
        }
        let op = match p.op {
            ShapingOp::Weld => BooleanOp::Union,
            ShapingOp::Trim => BooleanOp::Difference,
            ShapingOp::Intersect => BooleanOp::Intersect,
            ShapingOp::Simplify => BooleanOp::Exclude,
        };
        let said = iai_canvas::shaping::boolean_selected(canvas, op)
            .map_err(|e| not_enabled(e.clone(), e))?;
        let id = {
            let stack = &canvas.layer_stack;
            stack.layers.get(stack.active_idx).map(|l| l.id)
        };
        s.emit(EngineEvent::Structure { doc });
        s.emit(EngineEvent::Selection { doc });
        s.notice(said.clone(), said);
        super::layer::answer(s, doc, id).map(Plan::Done)
    }
}

// ---- PowerClip ------------------------------------------------------------

/// What `vector.powerclip` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PowerClipOp {
    /// Put the picked layers inside the top-most picked vector layer (the
    /// frame): they show only within its shape.
    Place,
    /// Take the picked content out of its frame, free again.
    Extract,
    /// Centre the picked content in its frame, its size kept.
    Center,
    /// Scale it to fit wholly inside the frame, centred.
    Fit,
    /// Scale it to cover the frame (what spills over is clipped), centred.
    Fill,
    /// Stretch it to exactly fill the frame.
    Stretch,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PowerClipParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    pub op: PowerClipOp,
    /// The layers (for `place`, the frame and what goes in it); the ones
    /// picked in the Layers panel when omitted.
    #[schemars(length(min = 1, max = 1000))]
    pub layers: Option<Vec<u32>>,
}
impl Validate for PowerClipParams {}

static POWERCLIP: CommandMeta = CommandMeta {
    id: "vector.powerclip",
    label_vi: "PowerClip (đặt vào khung)",
    label_en: "PowerClip",
    doc: "Place layers inside a vector frame so they show only within its shape (`place`: the \
          top-most picked vector layer is the frame), take them out again (`extract`), or \
          arrange the content in its frame (`center`, `fit`, `fill`, `stretch`). The clip \
          follows when the frame or the content moves. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct PowerClip;

impl Command for PowerClip {
    type Params = PowerClipParams;
    type Output = super::layer::LayerChanged;
    fn meta(&self) -> &'static CommandMeta {
        &POWERCLIP
    }
    fn plan(
        &self,
        s: &mut Session<'_>,
        p: PowerClipParams,
    ) -> Result<Plan<super::layer::LayerChanged>, CmdError> {
        use iai_canvas::powerclip::{self, Fit};
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        if let Some(ids) = &p.layers {
            let rows: Vec<usize> = ids
                .iter()
                .map(|id| crate::layer_index(canvas, Some(*id)))
                .collect::<Result<_, _>>()?;
            for (i, l) in canvas.layer_stack.layers.iter_mut().enumerate() {
                l.selected = rows.contains(&i);
            }
        }
        let no_content = || {
            not_enabled(
                "no picked layer is PowerClip content",
                "No PowerClip content selected",
            )
        };
        let said = match p.op {
            PowerClipOp::Place => {
                let n = super::layer::structural(canvas, "PowerClip", |canvas| {
                    powerclip::place(canvas).ok_or_else(|| {
                        not_enabled(
                            "pick a vector layer for the frame and a layer to put in it",
                            "Select content and a vector shape to use as the frame",
                        )
                    })
                })?;
                format!("Placed {n} object(s) into the PowerClip frame")
            }
            PowerClipOp::Extract => {
                let n = super::layer::structural(canvas, "Extract PowerClip", |canvas| {
                    match powerclip::extract(canvas) {
                        0 => Err(no_content()),
                        n => Ok(n),
                    }
                })?;
                format!("Extracted {n} object(s) from the frame")
            }
            op => {
                let (fit, line) = match op {
                    PowerClipOp::Center => (Fit::Center, "Centred content in the frame"),
                    PowerClipOp::Fit => (Fit::Fit, "Fit content inside the frame"),
                    PowerClipOp::Fill => (Fit::Fill, "Filled the frame with the content"),
                    _ => (Fit::Stretch, "Stretched content to fill the frame"),
                };
                if !powerclip::has_selected_content(&canvas.layer_stack) {
                    return Err(no_content());
                }
                super::layer::structural(canvas, "Arrange PowerClip", |canvas| {
                    match powerclip::arrange(canvas, fit) {
                        0 => Err(not_enabled(
                            "the frame or the content has nothing drawn",
                            "Nothing to arrange: the frame or the content is empty",
                        )),
                        _ => Ok(()),
                    }
                })?;
                line.to_string()
            }
        };
        let id = {
            let stack = &canvas.layer_stack;
            stack.layers.get(stack.active_idx).map(|l| l.id)
        };
        s.emit(EngineEvent::Selection { doc });
        s.notice(said.clone(), said);
        super::layer::answer(s, doc, id).map(Plan::Done)
    }
}
