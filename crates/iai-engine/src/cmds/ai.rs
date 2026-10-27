//! Local AI: cut the subject out of a layer, fill a selection with what
//! would be behind it, fetch a model.

use iai_ai::select_subject::{self, SelectSubjectModel};
use iai_canvas::canvas::selection_output::{SelectionOutput, output_selection};
use iai_doc::selection::Selection;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::common::{Bounds, not_enabled, parse_color};
use crate::{
    Apply, CmdError, Command, CommandMeta, Effect, EngineEvent, ErrorCode, Job, JobCtx, Plan,
    Session, Validate, layer_index,
};

/// What the subject model found.
pub struct Segmented {
    /// One byte per pixel, 0 (background) to 255 (subject).
    pub mask: Vec<u8>,
    pub on_gpu: bool,
}

/// Refusal when `model` is not on this machine, pointing at the download.
pub fn model_missing(model: SelectSubjectModel) -> CmdError {
    CmdError::new(
        ErrorCode::ModelMissing,
        format!(
            "the {} model is not installed ({})",
            model.name(),
            model.size_hint()
        ),
    )
    .vi(format!(
        "Chưa có model {} ({})",
        model.name(),
        model.size_hint()
    ))
    .with_hint(json!({"command": "ai.model_download", "args": {"model": "birefnet"}}))
}

/// The worker half of running `model` on `pixels`, its mask softened by
/// `feather_px`.
pub fn segment_work(
    model: SelectSubjectModel,
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    feather_px: f32,
) -> impl FnOnce(&JobCtx) -> Result<Segmented, CmdError> + Send + 'static {
    move |cx: &JobCtx| {
        cx.progress(0.05, format!("Đang nạp model {}…", model.name()));
        let loaded = || cx.progress(0.2, format!("Đang chạy {}…", model.name()));
        let (mut mask, on_gpu) =
            select_subject::segment_kept(model, &pixels, width, height, &loaded)
                .map_err(|e| CmdError::new(ErrorCode::Internal, e.clone()).vi(e))?;
        cx.check_cancelled()?;
        let expected = (width as usize).saturating_mul(height as usize);
        if mask.len() != expected {
            return Err(CmdError::internal(
                "the model gave a mask of the wrong size",
            ));
        }
        if feather_px > 0.0 {
            iai_doc::selection::blur_mask(&mut mask, width as usize, height as usize, feather_px);
        }
        cx.progress(0.95, "Xong, đang áp kết quả…");
        Ok(Segmented { mask, on_gpu })
    }
}

/// Where `ai.remove_background` puts the subject.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CutoutOutput {
    /// A layer mask on the layer: nothing is erased (default).
    #[default]
    Mask,
    /// Only the selection: the subject is selected.
    Selection,
    /// A copy of the layer above it with the background cleared; the layer
    /// itself is hidden.
    NewLayer,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RemoveBackgroundParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
    #[serde(default)]
    pub output: CutoutOutput,
    /// Soften the subject's edge by this many pixels.
    #[serde(default)]
    #[schemars(range(min = 0.0, max = 64.0))]
    pub feather_px: f32,
    /// Put a layer of this colour, "#RRGGBB", under the subject (ID-photo blue
    /// is "#0090FF"). Not with `output: selection`.
    pub background: Option<String>,
}

impl Validate for RemoveBackgroundParams {
    fn validate(&self) -> Result<(), CmdError> {
        if let Some(c) = &self.background {
            parse_color("background", c)?;
            if self.output == CutoutOutput::Selection {
                return Err(CmdError::bad_param(
                    "background",
                    "to be left out with output = selection",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RemoveBackgroundOutput {
    pub doc: u32,
    /// The layer holding the result: the layer itself, or its new copy.
    pub layer: u32,
    /// The colour layer put under it, when `background` was given.
    pub background_layer: Option<u32>,
    /// The subject's bounds on the page; none when nothing was found.
    pub subject_bounds: Option<Bounds>,
    /// Share of the layer kept, 0 to 1.
    pub coverage: f32,
    /// `gpu` or `cpu`.
    pub engine: String,
    pub model: String,
    pub model_sha256: String,
}

static REMOVE_BG: CommandMeta = CommandMeta {
    id: "ai.remove_background",
    label_vi: "Tách nền",
    label_en: "Remove Background",
    doc: "Cut the main subject (a person, a product) out of a layer with a local AI model \
          (BiRefNet; no network). Default output is a layer mask, so nothing is erased and it \
          can be undone. `background` puts a solid colour behind the subject (ID photos). \
          Takes seconds: run it with `background: true` for a job id at once.",
    effects: &[Effect::Edit, Effect::LocalAi],
    featured: true,
};

pub struct RemoveBackground;

const MODEL: SelectSubjectModel = SelectSubjectModel::BiRefNetTiny;

impl Command for RemoveBackground {
    type Params = RemoveBackgroundParams;
    type Output = RemoveBackgroundOutput;

    fn meta(&self) -> &'static CommandMeta {
        &REMOVE_BG
    }

    fn enabled(&self, s: &Session<'_>, p: &RemoveBackgroundParams) -> Result<(), CmdError> {
        let canvas = s.canvas(p.doc)?;
        let idx = layer_index(canvas, p.layer)?;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        if !layer.is_raster() {
            return Err(not_enabled(
                "the layer has no pixels of its own (text, shape, group); rasterize it first",
                "Layer này không phải layer ảnh, hãy rasterize trước",
            ));
        }
        if layer.width == 0 || layer.height == 0 {
            return Err(not_enabled("the layer is empty", "Layer trống"));
        }
        if !MODEL.is_installed() {
            return Err(model_missing(MODEL));
        }
        Ok(())
    }

    fn plan(
        &self,
        s: &mut Session<'_>,
        p: RemoveBackgroundParams,
    ) -> Result<Plan<RemoveBackgroundOutput>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        let (layer_id, lw, lh) = (layer.id, layer.width, layer.height);
        let pixels = layer.flatten_tiles();
        let tiles = layer.tiles.clone();
        let background = p
            .background
            .as_deref()
            .map(|c| parse_color("background", c))
            .transpose()?;
        let work = segment_work(MODEL, pixels, lw, lh, p.feather_px);
        let output = p.output;
        Ok(Plan::Job(Job {
            doc: Some(doc),
            label_vi: "Tách nền".into(),
            work: Box::new(move |cx| {
                let found = work(cx)?;
                Ok(Box::new(move |s: &mut Session<'_>| {
                    apply_cutout(s, doc, layer_id, &tiles, found, output, background)
                }) as Apply<RemoveBackgroundOutput>)
            }),
        }))
    }
}

/// The layer-space `mask` of a `lw × lh` layer at `offset`, as a selection of
/// a `cw × ch` page.
fn page_selection(
    mask: &[u8],
    (lw, lh): (u32, u32),
    offset: (i32, i32),
    (cw, ch): (u32, u32),
) -> Selection {
    let mut sel = Selection::new(cw, ch);
    let (lw, lh) = (lw as usize, lh as usize);
    let (cw_us, ch_us) = (cw as usize, ch as usize);
    for (ly, row) in mask.chunks(lw.max(1)).take(lh).enumerate() {
        let Some(cy) = (ly as i64)
            .checked_add(i64::from(offset.1))
            .filter(|&y| y >= 0 && (y as usize) < ch_us)
        else {
            continue;
        };
        for (lx, &m) in row.iter().enumerate() {
            if m == 0 {
                continue;
            }
            let Some(cx) = (lx as i64)
                .checked_add(i64::from(offset.0))
                .filter(|&x| x >= 0 && (x as usize) < cw_us)
            else {
                continue;
            };
            let at = (cy as usize)
                .checked_mul(cw_us)
                .and_then(|r| r.checked_add(cx as usize));
            if let Some(px) = at.and_then(|i| sel.mask.get_mut(i)) {
                *px = m;
            }
        }
    }
    sel.active = sel.mask.iter().any(|&v| v > 0);
    sel.mask_revision = sel.mask_revision.saturating_add(1);
    sel.mark_bbox_dirty();
    sel
}

fn apply_cutout(
    s: &mut Session<'_>,
    doc: u32,
    layer_id: u32,
    tiles_then: &iai_raster::tile::TileMap,
    found: Segmented,
    output: CutoutOutput,
    background: Option<[u8; 4]>,
) -> Result<RemoveBackgroundOutput, CmdError> {
    let canvas = s.canvas_mut(Some(doc))?;
    let idx = layer_index(canvas, Some(layer_id))?;
    let (lw, lh, offset, same) = {
        let l = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        (l.width, l.height, l.offset, l.tiles.same_tiles(tiles_then))
    };
    if !same || found.mask.len() != (lw as usize).saturating_mul(lh as usize) {
        return Err(CmdError::new(
            ErrorCode::StaleRevision,
            "the layer changed while the model ran; run it again",
        )
        .vi("Layer đã đổi trong lúc chạy — hãy chạy lại"));
    }
    let kept: u64 = found.mask.iter().map(|&m| u64::from(m)).sum();
    let total = (found.mask.len() as u64).saturating_mul(255).max(1);
    let coverage = (kept as f64 / total as f64) as f32;
    let sel = page_selection(&found.mask, (lw, lh), offset, (canvas.width, canvas.height));
    let mut bounds_sel = sel.clone();
    let subject_bounds = if bounds_sel.active {
        let (x0, y0, x1, y1) = bounds_sel.bounding_box();
        Bounds::from_corners(x0, y0, x1, y1)
    } else {
        None
    };
    let (cw, ch) = (canvas.width, canvas.height);

    let mut result_layer = layer_id;
    let mut background_layer = None;
    match output {
        CutoutOutput::Selection => {
            let mut cmd = iai_history::command::SelectionCommand::capture_before(
                "Tách nền",
                &canvas.selection,
            );
            canvas.selection = sel;
            cmd.capture_after(&canvas.selection);
            canvas.record_as(Box::new(cmd), iai_history::gateway::ChangeKind::Selection);
        }
        CutoutOutput::Mask | CutoutOutput::NewLayer => {
            let mut cmd = iai_history::command::LayerStructureCommand::capture_before(
                "Tách nền",
                &canvas.layer_stack,
                cw,
                ch,
            );
            canvas.layer_stack.active_idx = idx;
            let mode = if output == CutoutOutput::Mask {
                SelectionOutput::LayerMask
            } else {
                SelectionOutput::NewLayer
            };
            if !output_selection(canvas, &sel, mode, None) {
                return Err(not_enabled(
                    "the subject could not be written to this layer",
                    "Không ghi được kết quả vào layer này",
                ));
            }
            let at = canvas.layer_stack.active_idx;
            result_layer = canvas
                .layer_stack
                .layers
                .get(at)
                .map(|l| l.id)
                .unwrap_or(layer_id);
            if let Some(rgba) = background {
                background_layer = Some(insert_colour_below(canvas, at, rgba));
            }
            cmd.capture_after(&canvas.layer_stack, cw, ch);
            canvas.record_as(
                Box::new(cmd),
                iai_history::gateway::ChangeKind::LayerStructure,
            );
            canvas.layer_revision = canvas.layer_revision.saturating_add(1);
            canvas.flatten_full();
            s.emit(EngineEvent::Structure { doc });
        }
    }
    s.emit(EngineEvent::Selection { doc });
    let engine = if found.on_gpu { "gpu" } else { "cpu" };
    s.notice(
        format!("Tách nền xong ({})", engine.to_ascii_uppercase()),
        format!("Background removed ({engine})"),
    );
    Ok(RemoveBackgroundOutput {
        doc,
        layer: result_layer,
        background_layer,
        subject_bounds,
        coverage,
        engine: engine.to_string(),
        model: MODEL.name().to_string(),
        model_sha256: MODEL.sha256().to_string(),
    })
}

/// A page-sized layer of `rgba` right under the layer at `at`, in its
/// folder; its id. The active layer stays the one at `at`.
fn insert_colour_below(canvas: &mut iai_canvas::canvas::Canvas, at: usize, rgba: [u8; 4]) -> u32 {
    let stack = &mut canvas.layer_stack;
    let id = stack.next_id();
    stack.set_next_id(id.saturating_add(1));
    let [r, g, b, a] = rgba;
    let mut layer = iai_doc::layer::Layer::new(id, "Nền", canvas.width, canvas.height);
    layer.tiles = iai_raster::tile::TileMap::new_solid(canvas.width, canvas.height, r, g, b, a);
    layer.parent_id = stack.layers.get(at).and_then(|l| l.parent_id);
    let at = at.min(stack.layers.len());
    stack.layers.insert(at, layer);
    stack.active_idx = at
        .saturating_add(1)
        .min(stack.layers.len().saturating_sub(1));
    stack.repair_next_id();
    id
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelName {
    /// BiRefNet (~214 MB): Select Subject, Remove Background, ID photos.
    #[default]
    Birefnet,
    /// LaMa (~200 MB): AI fill (`ai.inpaint`), Repair Brush.
    Lama,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DownloadParams {
    pub model: ModelName,
}
impl Validate for DownloadParams {}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Downloaded {
    pub model: String,
    pub installed: bool,
}

static DOWNLOAD: CommandMeta = CommandMeta {
    id: "ai.model_download",
    label_vi: "Tải model AI",
    label_en: "Download AI Model",
    doc: "Download a local AI model once (hundreds of MB), checked by its SHA-256. Needs the \
          network: an agent may run it only when launched with --allow-network.",
    effects: &[Effect::Network, Effect::WriteFiles],
    featured: false,
};

pub struct ModelDownload;

impl Command for ModelDownload {
    type Params = DownloadParams;
    type Output = Downloaded;

    fn meta(&self) -> &'static CommandMeta {
        &DOWNLOAD
    }

    fn plan(&self, _: &mut Session<'_>, p: DownloadParams) -> Result<Plan<Downloaded>, CmdError> {
        let name = match p.model {
            ModelName::Birefnet => "birefnet",
            ModelName::Lama => "lama",
        };
        let installed = match p.model {
            ModelName::Birefnet => MODEL.is_installed(),
            ModelName::Lama => iai_ai::lama::is_available(),
        };
        if installed {
            return Ok(Plan::Done(Downloaded {
                model: name.to_string(),
                installed: true,
            }));
        }
        Ok(Plan::Job(Job {
            doc: None,
            label_vi: format!("Tải model {name}"),
            work: Box::new(move |cx| {
                match p.model {
                    ModelName::Birefnet => {
                        let progress =
                            |f: f32| cx.progress(f, format!("Đang tải {}…", MODEL.name()));
                        select_subject::download_model(MODEL, &progress)
                            .map_err(|e| CmdError::new(ErrorCode::Io, e.clone()).vi(e))?;
                    }
                    ModelName::Lama => {
                        iai_ai::lama::ensure_downloading();
                        while iai_ai::lama::is_downloading() {
                            cx.check_cancelled()?;
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        if !iai_ai::lama::is_available() {
                            let why = iai_ai::lama::status_text()
                                .unwrap_or_else(|| "download failed".to_string());
                            return Err(CmdError::new(ErrorCode::Io, why.clone()).vi(why));
                        }
                    }
                }
                Ok(Box::new(move |_: &mut Session<'_>| {
                    Ok(Downloaded {
                        model: name.to_string(),
                        installed: true,
                    })
                }) as Apply<Downloaded>)
            }),
        }))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct InpaintParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Layer id (see `doc.inspect`); the active layer when omitted.
    pub layer: Option<u32>,
}
impl Validate for InpaintParams {}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Inpainted {
    pub doc: u32,
    pub layer: u32,
    /// `lama` (the AI model) or `patch` (texture from the rest of the photo).
    pub method: String,
    pub pixels_filled: u64,
}

static INPAINT: CommandMeta = CommandMeta {
    id: "ai.inpaint",
    label_vi: "Xóa vật thể (AI)",
    label_en: "Remove Object (AI fill)",
    doc: "Fill the selected part of a layer with what would be behind it: the LaMa model when \
          installed, else texture from the rest of the photo. Select the object first \
          (`select.rect`, `select.subject`). One undo step.",
    effects: &[Effect::Edit, Effect::LocalAi],
    featured: true,
};

pub struct Inpaint;

impl Command for Inpaint {
    type Params = InpaintParams;
    type Output = Inpainted;

    fn meta(&self) -> &'static CommandMeta {
        &INPAINT
    }

    fn enabled(&self, s: &Session<'_>, p: &InpaintParams) -> Result<(), CmdError> {
        let canvas = s.canvas(p.doc)?;
        if !canvas.selection.active {
            return Err(not_enabled(
                "nothing is selected: select what to remove first",
                "Hãy chọn vùng cần xóa trước",
            )
            .with_hint(json!({"command": "select.rect"})));
        }
        let idx = layer_index(canvas, p.layer)?;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        if !layer.is_raster() || (layer.locked && !layer.is_background) {
            return Err(not_enabled(
                "the layer is not a raster layer, or it is locked",
                "Layer này không phải layer ảnh, hoặc đang khóa",
            ));
        }
        Ok(())
    }

    fn plan(&self, s: &mut Session<'_>, p: InpaintParams) -> Result<Plan<Inpainted>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas(Some(doc))?;
        let idx = layer_index(canvas, p.layer)?;
        let layer = canvas
            .layer_stack
            .layers
            .get(idx)
            .ok_or_else(|| CmdError::internal("layer index"))?;
        let (layer_id, lw, lh, (ox, oy)) = (layer.id, layer.width, layer.height, layer.offset);
        let before = layer.tiles.clone();
        let (cw, ch) = (canvas.width as i64, canvas.height as i64);
        let (lw_us, lh_us) = (lw as usize, lh as usize);
        let mut hole = vec![false; lw_us.saturating_mul(lh_us)];
        let mut filled = 0u64;
        for (i, cell) in hole.iter_mut().enumerate() {
            let row = lw_us.max(1);
            let (x, y) = (
                i.checked_rem(row).unwrap_or(0) as i64,
                i.checked_div(row).unwrap_or(0) as i64,
            );
            let (cx, cy) = (
                x.saturating_add(i64::from(ox)),
                y.saturating_add(i64::from(oy)),
            );
            if cx < 0 || cy < 0 || cx >= cw || cy >= ch {
                continue;
            }
            if canvas.selection.sample(cx as u32, cy as u32) > 0.5 {
                *cell = true;
                filled = filled.saturating_add(1);
            }
        }
        if filled == 0 {
            return Err(not_enabled(
                "the selection does not cover this layer",
                "Vùng chọn không nằm trên layer này",
            ));
        }
        let pixels = before.flatten();
        Ok(Plan::Job(Job {
            doc: Some(doc),
            label_vi: "Xóa vật thể".into(),
            work: Box::new(move |cx| {
                let mut buf = pixels;
                cx.progress(0.1, "Đang lấp vùng chọn…");
                let used_ai = iai_ai::lama::inpaint(&mut buf, lw_us, lh_us, &hole);
                if used_ai {
                    iai_ai::smart_fill::refine_fill(&mut buf, lw_us, lh_us, &hole);
                } else if !iai_ai::smart_fill::fill(&mut buf, lw_us, lh_us, &hole) {
                    return Err(not_enabled(
                        "nothing around the selection to fill it from",
                        "Không có gì xung quanh để lấp vùng chọn",
                    ));
                }
                cx.check_cancelled()?;
                let after = iai_raster::tile::TileMap::from_rgba(&buf, lw, lh);
                Ok(Box::new(move |s: &mut Session<'_>| {
                    let canvas = s.canvas_mut(Some(doc))?;
                    let idx = layer_index(canvas, Some(layer_id))?;
                    let same = canvas
                        .layer_stack
                        .layers
                        .get(idx)
                        .is_some_and(|l| l.tiles.same_tiles(&before));
                    if !same {
                        return Err(CmdError::new(
                            ErrorCode::StaleRevision,
                            "the layer changed while it was being filled; run it again",
                        )
                        .vi("Layer đã đổi trong lúc xử lý — hãy chạy lại"));
                    }
                    let label = if used_ai {
                        "Smart Fill (AI)"
                    } else {
                        "Smart Fill"
                    };
                    if !canvas.commit_layer_tiles_change(layer_id, before, after, label) {
                        return Err(CmdError::internal("the fill could not be committed"));
                    }
                    s.emit(EngineEvent::Pixels { doc });
                    s.notice(
                        if used_ai {
                            "Đã xóa vật thể (AI)"
                        } else {
                            "Đã lấp vùng chọn"
                        },
                        "Selection filled",
                    );
                    Ok(Inpainted {
                        doc,
                        layer: layer_id,
                        method: if used_ai { "lama" } else { "patch" }.to_string(),
                        pixels_filled: filled,
                    })
                }) as Apply<Inpainted>)
            }),
        }))
    }
}
