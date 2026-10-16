//! The selection: all, none, inverted, a rectangle, the main subject (AI).

use iai_ai::select_subject::SelectSubjectModel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ai::{model_missing, segment_work, Segmented};
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
    ($ty:ident, $meta:ident, $id:literal, $vi:literal, $en:literal, $doc:literal, $op:ident, $say:expr) => {
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
