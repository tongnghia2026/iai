//! The selection: all, none, inverted, a rectangle, the main subject (AI).

use iai_ai::select_subject::SelectSubjectModel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ai::{Segmented, model_missing, segment_work};
use super::common::{Bounds, DocParams, RectPx, SelectMode};
use crate::{
    CmdError, Command, CommandMeta, Effect, EngineEvent, ErrorCode, Job, Plan, Session, Validate,
};

/// What a selection command answers.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Selected {
    pub doc: u32,
    /// Something is selected.
    pub active: bool,
    pub bounds: Option<Bounds>,
}

pub(crate) fn selected(s: &mut Session<'_>, doc: u32) -> Result<Selected, CmdError> {
    let canvas = s.canvas_mut(Some(doc))?;
    let active = canvas.selection.active;
    let bounds = if active {
        let (x0, y0, x1, y1) = canvas.selection.bounding_box();
        Bounds::from_corners(x0, y0, x1, y1)
    } else {
        None
    };
    s.emit(EngineEvent::Selection { doc });
    Ok(Selected {
        doc,
        active,
        bounds,
    })
}

macro_rules! simple_select {
    ($ty:ident, $meta:ident, $id:literal, $vi:literal, $en:literal, $doc:literal, $op:ident, $say:expr_2021) => {
        static $meta: CommandMeta = CommandMeta {
            id: $id,
            label_vi: $vi,
            label_en: $en,
            doc: $doc,
            effects: &[Effect::Edit],
            featured: false,
        };

        pub struct $ty;

        impl Command for $ty {
            type Params = DocParams;
            type Output = Selected;
            fn meta(&self) -> &'static CommandMeta {
                &$meta
            }
            fn plan(&self, s: &mut Session<'_>, p: DocParams) -> Result<Plan<Selected>, CmdError> {
                let doc = s.doc_id(p.doc)?;
                let canvas = s.canvas_mut(Some(doc))?;
                canvas.$op();
                let say: fn(u32, u32) -> Option<String> = $say;
                if let Some(line) = say(canvas.width, canvas.height) {
                    s.notice(line.clone(), line);
                }
                selected(s, doc).map(Plan::Done)
            }
        }
    };
}

simple_select!(
    SelectAll,
    ALL,
    "select.all",
    "Chọn tất cả",
    "Select All",
    "Select the whole page.",
    select_all,
    |w, h| Some(format!("Đã chọn tất cả ({w}×{h})"))
);
simple_select!(
    SelectNone,
    NONE,
    "select.none",
    "Bỏ chọn",
    "Deselect",
    "Clear the selection.",
    deselect,
    |_, _| None
);
simple_select!(
    SelectInvert,
    INVERT,
    "select.invert",
    "Đảo vùng chọn",
    "Invert Selection",
    "Select what is not selected, and the other way round.",
    invert_selection,
    |_, _| None
);

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RectParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The rectangle, in document pixels; the part outside the page is left out.
    pub rect: RectPx,
    #[serde(default)]
    pub mode: SelectMode,
    /// Soften the edge by this many pixels.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 500.0))]
    pub feather_px: f32,
}
impl Validate for RectParams {}

static RECT: CommandMeta = CommandMeta {
    id: "select.rect",
    label_vi: "Chọn vùng chữ nhật",
    label_en: "Select Rectangle",
    doc: "Select a rectangle of the page, replacing the selection or added to, taken from or \
          intersected with it.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct SelectRect;

impl Command for SelectRect {
    type Params = RectParams;
    type Output = Selected;
    fn meta(&self) -> &'static CommandMeta {
        &RECT
    }
    fn plan(&self, s: &mut Session<'_>, p: RectParams) -> Result<Plan<Selected>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let Some((x0, y0, x1, y1)) = p.rect.clipped(canvas.width, canvas.height) else {
            return Err(CmdError::new(
                ErrorCode::BadParams,
                format!(
                    "the rectangle lies outside the {}×{} page",
                    canvas.width, canvas.height
                ),
            )
            .vi("Vùng chọn nằm ngoài ảnh"));
        };
        canvas.select_rect_mode(x0, y0, x1, y1, p.mode.to_core(), p.feather_px, true);
        selected(s, doc).map(Plan::Done)
    }
}

static ELLIPSE: CommandMeta = CommandMeta {
    id: "select.ellipse",
    label_vi: "Chọn vùng elip",
    label_en: "Select Ellipse",
    doc: "Select the ellipse (a circle when the box is square) that fills a box of the page, as \
          the Elliptical Marquee does; it replaces the selection or is added to, taken from or \
          intersected with it. The part outside the page is left out.",
    effects: &[Effect::Edit],
    featured: false,
};

/// An ellipse as a closed outline of `n` points, for one the page cuts.
fn ellipse_outline(r: &RectPx, n: usize) -> Vec<(f32, f32)> {
    let (rx, ry) = (r.w as f32 / 2.0, r.h as f32 / 2.0);
    let (cx, cy) = (r.x as f32 + rx, r.y as f32 + ry);
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32 * std::f32::consts::TAU;
            (cx + rx * t.cos(), cy + ry * t.sin())
        })
        .collect()
}

pub struct SelectEllipse;

impl Command for SelectEllipse {
    type Params = RectParams;
    type Output = Selected;
    fn meta(&self) -> &'static CommandMeta {
        &ELLIPSE
    }
    fn plan(&self, s: &mut Session<'_>, p: RectParams) -> Result<Plan<Selected>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let Some((x0, y0, x1, y1)) = p.rect.clipped(canvas.width, canvas.height) else {
            return Err(CmdError::new(
                ErrorCode::BadParams,
                format!(
                    "the box lies outside the {}×{} page",
                    canvas.width, canvas.height
                ),
            )
            .vi("Vùng chọn nằm ngoài ảnh"));
        };
        let whole = x1.saturating_sub(x0) == p.rect.w && y1.saturating_sub(y0) == p.rect.h;
        if whole {
            canvas.select_ellipse_mode(x0, y0, x1, y1, p.mode.to_core(), p.feather_px, true);
        } else {
            // The page cuts it: its outline, drawn where it lies.
            let outline = ellipse_outline(&p.rect, 512);
            canvas.select_polygon_mode(&outline, p.mode.to_core(), p.feather_px, true);
        }
        selected(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PolygonParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The corners `[x, y]` in document pixels, in order round the shape;
    /// the last joins the first.
    #[schemars(length(min = 3, max = 10000))]
    pub points: Vec<[f32; 2]>,
    #[serde(default)]
    pub mode: SelectMode,
    /// Soften the edge by this many pixels.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 500.0))]
    pub feather_px: f32,
}

impl Validate for PolygonParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.points.len() < 3 {
            return Err(CmdError::bad_param("points", "at least three corners"));
        }
        if self
            .points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > 100_000.0)
        {
            return Err(CmdError::bad_param(
                "points",
                "numbers between -100000 and 100000",
            ));
        }
        Ok(())
    }
}

static POLYGON: CommandMeta = CommandMeta {
    id: "select.polygon",
    label_vi: "Chọn vùng đa giác",
    label_en: "Select Polygon",
    doc: "Select the shape inside a closed outline of points, as the Polygonal Lasso does; it \
          replaces the selection or is added to, taken from or intersected with it. The part \
          outside the page is left out.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct SelectPolygon;

impl Command for SelectPolygon {
    type Params = PolygonParams;
    type Output = Selected;
    fn meta(&self) -> &'static CommandMeta {
        &POLYGON
    }
    fn plan(&self, s: &mut Session<'_>, p: PolygonParams) -> Result<Plan<Selected>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let (w, h) = (canvas.width as f32, canvas.height as f32);
        let points: Vec<(f32, f32)> = p.points.iter().map(|[x, y]| (*x, *y)).collect();
        let (lo_x, hi_x) = points
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), (x, _)| {
                (lo.min(*x), hi.max(*x))
            });
        let (lo_y, hi_y) = points
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), (_, y)| {
                (lo.min(*y), hi.max(*y))
            });
        if hi_x <= 0.0 || hi_y <= 0.0 || lo_x >= w || lo_y >= h {
            return Err(CmdError::new(
                ErrorCode::BadParams,
                format!("the shape lies outside the {w}×{h} page"),
            )
            .vi("Vùng chọn nằm ngoài ảnh"));
        }
        canvas.select_polygon_mode(&points, p.mode.to_core(), p.feather_px, true);
        selected(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SubjectParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    #[serde(default)]
    pub mode: SelectMode,
}
impl Validate for SubjectParams {}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubjectSelected {
    #[serde(flatten)]
    pub selection: Selected,
    /// Where the model ran: `gpu` or `cpu`.
    pub engine: String,
    pub model: String,
    pub model_sha256: String,
}

static SUBJECT: CommandMeta = CommandMeta {
    id: "select.subject",
    label_vi: "Chọn chủ thể",
    label_en: "Select Subject",
    doc: "Select the main subject (a person, a product) of what the page shows, with the local \
          BiRefNet model (no network). Takes seconds; run it with `background: true` to get a \
          job id at once.",
    effects: &[Effect::Edit, Effect::LocalAi],
    featured: false,
};

pub struct SelectSubject;

const MODEL: SelectSubjectModel = SelectSubjectModel::BiRefNetTiny;

impl Command for SelectSubject {
    type Params = SubjectParams;
    type Output = SubjectSelected;
    fn meta(&self) -> &'static CommandMeta {
        &SUBJECT
    }

    fn enabled(&self, s: &Session<'_>, p: &SubjectParams) -> Result<(), CmdError> {
        s.canvas(p.doc)?;
        if !MODEL.is_installed() {
            return Err(model_missing(MODEL));
        }
        Ok(())
    }

    fn plan(
        &self,
        s: &mut Session<'_>,
        p: SubjectParams,
    ) -> Result<Plan<SubjectSelected>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        canvas.ensure_pixels();
        let (w, h) = (canvas.width, canvas.height);
        let pixels = if canvas.pixels.is_empty() {
            canvas.flatten_for_export()
        } else {
            canvas.pixels.clone()
        };
        let mode = p.mode.to_core();
        let work = segment_work(MODEL, pixels, w, h, 0.0);
        Ok(Plan::Job(Job {
            doc: Some(doc),
            label_vi: "Chọn chủ thể".into(),
            work: Box::new(move |cx| {
                let found = work(cx)?;
                Ok(
                    Box::new(move |s: &mut Session<'_>| apply_subject(s, doc, (w, h), found, mode))
                        as crate::Apply<SubjectSelected>,
                )
            }),
        }))
    }
}

fn apply_subject(
    s: &mut Session<'_>,
    doc: u32,
    size: (u32, u32),
    found: Segmented,
    mode: iai_doc::selection::SelectionMode,
) -> Result<SubjectSelected, CmdError> {
    let shown = s.active_id() == Some(doc);
    let title = s.document(Some(doc))?.tab_title().to_string();
    let canvas = s.canvas_mut(Some(doc))?;
    if (canvas.width, canvas.height) != size || canvas.selection.mask.len() != found.mask.len() {
        return Err(CmdError::new(
            ErrorCode::StaleRevision,
            "the image changed size while the model ran; run it again",
        )
        .vi("Kích thước ảnh đã đổi trong lúc chạy — hãy chạy lại"));
    }
    let mut cmd =
        iai_history::command::SelectionCommand::capture_before("Select Subject", &canvas.selection);
    canvas.selection.apply_with_mode(found.mask, mode);
    canvas.selection.mark_bbox_dirty();
    cmd.capture_after(&canvas.selection);
    canvas.record_as(Box::new(cmd), iai_history::gateway::ChangeKind::Selection);
    canvas.selection.refresh_bbox();
    let engine = if found.on_gpu { "GPU" } else { "CPU" };
    if shown {
        s.notice(
            format!("Select Subject xong ({engine})"),
            format!("Subject selected ({engine})"),
        );
    } else {
        s.notice(
            format!("Select Subject xong ({engine}) ở tab \"{title}\" — chuyển qua tab đó để xem"),
            format!("Subject selected ({engine}) in \"{title}\""),
        );
    }
    let selection = selected(s, doc)?;
    Ok(SubjectSelected {
        selection,
        engine: engine.to_ascii_lowercase(),
        model: MODEL.name().to_string(),
        model_sha256: MODEL.sha256().to_string(),
    })
}
