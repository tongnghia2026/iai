//! The designer's object tools beyond Shaping (CorelDRAW's Contour, Convert
//! Outline to Object, Combine, Break Apart), each one undo step. The work is
//! in `iai_canvas::design`.

use iai_canvas::design::{self, ContourSpec};
use iai_doc::vector::style::LineJoin;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{not_enabled, parse_color};
use crate::{CmdError, Command, CommandMeta, Effect, EngineEvent, Plan, Session, Validate};

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Designed {
    pub doc: u32,
    /// The layers made, in the order they were made.
    pub made: Vec<u32>,
}

/// After a change: tell the windows, say what was done, answer.
fn done(s: &mut Session<'_>, doc: u32, made: Vec<u32>, said: String) -> Plan<Designed> {
    s.emit(EngineEvent::Structure { doc });
    s.emit(EngineEvent::Selection { doc });
    s.notice(said.clone(), said);
    Plan::Done(Designed { doc, made })
}

/// The picked layers when none are named.
fn ids_or_picked(
    s: &mut Session<'_>,
    doc: u32,
    ids: Option<Vec<u32>>,
) -> Result<Vec<u32>, CmdError> {
    if let Some(ids) = ids {
        return Ok(ids);
    }
    let canvas = s.canvas(Some(doc))?;
    let picked: Vec<u32> = canvas
        .layer_stack
        .layers
        .iter()
        .filter(|l| l.selected)
        .map(|l| l.id)
        .collect();
    if picked.is_empty() {
        let stack = &canvas.layer_stack;
        return Ok(stack
            .layers
            .get(stack.active_idx)
            .map(|l| l.id)
            .into_iter()
            .collect());
    }
    Ok(picked)
}

fn refused(e: String) -> CmdError {
    not_enabled(e.clone(), e)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    /// Round corners (a sticker's border, letters' outline).
    #[default]
    Round,
    /// Sharp corners, cut off when very sharp.
    Sharp,
    /// Corners cut flat.
    Bevel,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContourParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The vector layers; the picked ones when omitted.
    pub layers: Option<Vec<u32>>,
    /// Pixels between steps: outward when positive, inward when negative.
    #[schemars(range(min = -2000.0, max = 2000.0))]
    pub distance: f32,
    /// How many outlines (1 when omitted).
    #[serde(default = "one")]
    #[schemars(range(min = 1, max = 50))]
    pub steps: u32,
    #[serde(default)]
    pub corners: Corner,
    /// The fill of each step in turn, "#RRGGBB" (cycled); the object's own
    /// fill when omitted. E.g. ["#FFFFFF"] for a sticker's white border,
    /// ["#FFFFFF", "#003399"] for letters outlined white then blue.
    pub colors: Option<Vec<String>>,
    /// Give the contours the object's outline too.
    #[serde(default)]
    pub keep_outline: bool,
}

impl Validate for ContourParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.distance.abs() < 0.01 {
            return Err(CmdError::bad_param("distance", "not 0"));
        }
        for c in self.colors.iter().flatten() {
            parse_color("colors", c)?;
        }
        Ok(())
    }
}

static CONTOUR: CommandMeta = CommandMeta {
    id: "vector.contour",
    label_vi: "Nới / thu viền (Contour)",
    label_en: "Contour",
    doc: "CorelDRAW's Contour / Illustrator's Offset Path: outlines parallel to a vector \
          layer's shape, `distance` apart, outward (each a filled layer under it, the widest \
          lowest: a sticker's border, letters outlined in several colours, a decal's cutting \
          margin) or inward (above it: inner lines, a frame inside a badge). One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Contour;

impl Command for Contour {
    type Params = ContourParams;
    type Output = Designed;
    fn meta(&self) -> &'static CommandMeta {
        &CONTOUR
    }
    fn plan(&self, s: &mut Session<'_>, p: ContourParams) -> Result<Plan<Designed>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let ids = ids_or_picked(s, doc, p.layers)?;
        let colors = p
            .colors
            .iter()
            .flatten()
            .map(|c| parse_color("colors", c))
            .collect::<Result<Vec<_>, _>>()?;
        let spec = ContourSpec {
            distance: p.distance,
            steps: p.steps,
            join: match p.corners {
                Corner::Round => LineJoin::Round,
                Corner::Sharp => LineJoin::Miter,
                Corner::Bevel => LineJoin::Bevel,
            },
            colors,
            keep_outline: p.keep_outline,
        };
        let canvas = s.canvas_mut(Some(doc))?;
        let made = design::contour(canvas, &ids, &spec).map_err(refused)?;
        let said = format!("Made {} contour(s)", made.len());
        Ok(done(s, doc, made, said))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LayersParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The vector layers; the picked ones when omitted.
    pub layers: Option<Vec<u32>>,
}
impl Validate for LayersParams {}

static OUTLINE: CommandMeta = CommandMeta {
    id: "vector.outline_to_object",
    label_vi: "Đổi nét thành mảng",
    label_en: "Convert Outline to Object",
    doc: "Turn each vector layer's outline into a filled shape of its own, just above it, \
          painted with the outline's colour; the layer keeps only its fill (or goes when it had \
          none). What cutters, engravers and print shops need: no strokes, only shapes. One \
          undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct OutlineToObject;

impl Command for OutlineToObject {
    type Params = LayersParams;
    type Output = Designed;
    fn meta(&self) -> &'static CommandMeta {
        &OUTLINE
    }
    fn plan(&self, s: &mut Session<'_>, p: LayersParams) -> Result<Plan<Designed>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let ids = ids_or_picked(s, doc, p.layers)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let made = design::outline_to_object(canvas, &ids).map_err(refused)?;
        let said = format!("Turned {} outline(s) into shapes", made.len());
        Ok(done(s, doc, made, said))
    }
}

static COMBINE: CommandMeta = CommandMeta {
    id: "vector.combine",
    label_vi: "Gộp đường (Combine)",
    label_en: "Combine",
    doc: "Put several vector layers into one path (CorelDRAW Combine / Illustrator Compound \
          Path): overlaps cut holes, the bottom layer's look is kept — the letters of a word or \
          a shape with windows as one object. Unlike Weld nothing is merged away. One undo \
          step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct Combine;

impl Command for Combine {
    type Params = LayersParams;
    type Output = Designed;
    fn meta(&self) -> &'static CommandMeta {
        &COMBINE
    }
    fn plan(&self, s: &mut Session<'_>, p: LayersParams) -> Result<Plan<Designed>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let ids = ids_or_picked(s, doc, p.layers)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let made = design::combine(canvas, &ids).map_err(refused)?;
        Ok(done(
            s,
            doc,
            vec![made],
            format!("Combined {} layers", ids.len()),
        ))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LayerParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The vector layer; the active one when omitted.
    pub layer: Option<u32>,
}
impl Validate for LayerParams {}

static BREAK: CommandMeta = CommandMeta {
    id: "vector.break_apart",
    label_vi: "Tách rời (Break Apart)",
    label_en: "Break Apart",
    doc: "Split a vector layer into its separate pieces, one layer each, in reading order; a \
          piece keeps its own holes (the inside of an «o» stays a hole). One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct BreakApart;

impl Command for BreakApart {
    type Params = LayerParams;
    type Output = Designed;
    fn meta(&self) -> &'static CommandMeta {
        &BREAK
    }
    fn plan(&self, s: &mut Session<'_>, p: LayerParams) -> Result<Plan<Designed>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let id = ids_or_picked(s, doc, p.layer.map(|l| vec![l]))?
            .first()
            .copied()
            .ok_or_else(|| refused("Pick a vector layer".to_string()))?;
        let canvas = s.canvas_mut(Some(doc))?;
        let made = design::break_apart(canvas, id).map_err(refused)?;
        let said = format!("Broke apart into {} pieces", made.len());
        Ok(done(s, doc, made, said))
    }
}
