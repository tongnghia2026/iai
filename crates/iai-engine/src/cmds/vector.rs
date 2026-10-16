//! Shapes and paths that stay editable: their geometry and style are kept,
//! their pixels made from them.

use iai_doc::shape::{ShapeData, ShapeKind};
use iai_doc::vector::object::VectorObjectData;
use iai_doc::vector::path::{Contour, FillRule, Node, PathData};
use iai_doc::vector::style::VectorStyle;
use iai_geom::geometry::Point;
use iai_geom::vector::affine::AffineTransform;
use iai_history::command::LayerStructureCommand;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{not_enabled, parse_color, Bounds, RectPx};
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

/// One run of a path: points joined by straight lines.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Run {
    /// `[x, y]` in document pixels.
    #[schemars(length(min = 2, max = 100000))]
    pub points: Vec<[f32; 2]>,
    /// Join the last point back to the first.
    #[serde(default)]
    pub closed: bool,
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
    doc: "Draw a path of straight segments (one or more runs of points, open or closed) as a \
          new editable vector layer, filled and/or outlined.",
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
        let contours = p
            .runs
            .iter()
            .map(|run| {
                let nodes = run
                    .points
                    .iter()
                    .map(|[x, y]| Node::sharp(Point::new(*x, *y)))
                    .collect();
                Contour::new(nodes, run.closed)
            })
            .collect();
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
        let style = VectorStyle::from_shape_fields(
            fill.is_some(),
            fill.unwrap_or([0, 0, 0, 0]),
            stroke_width,
            stroke.unwrap_or([0, 0, 0, 0]),
        );
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
