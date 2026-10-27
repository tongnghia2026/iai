//! Alpha channels (the Channels panel): a selection kept as a channel, and
//! a channel loaded back as a selection. Channels are named by their place
//! in the panel, 0 being the first alpha channel.

use iai_canvas::canvas::Canvas;
use iai_doc::channels::{AlphaChannel, ChannelsState};
use iai_history::command::{AlphaChannelCommand, Command as _, EditContext, SelectionCommand};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::{SelectMode, not_enabled};
use super::select::{Selected, selected};
use crate::{CmdError, Command, CommandMeta, Effect, EngineEvent, Plan, Session, Validate};

/// The channels after a change.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Channels {
    pub doc: u32,
    /// The alpha channels' names, in the panel's order.
    pub channels: Vec<String>,
}

fn channels(s: &mut Session<'_>, doc: u32) -> Result<Channels, CmdError> {
    let canvas = s.canvas(Some(doc))?;
    let names = canvas
        .channels
        .alpha
        .iter()
        .map(|c| c.name.clone())
        .collect();
    s.emit(EngineEvent::Structure { doc });
    Ok(Channels {
        doc,
        channels: names,
    })
}

/// Run `cmd` on `canvas`'s channels and keep it as an undo step.
fn record(canvas: &mut Canvas, mut cmd: AlphaChannelCommand) -> Result<(), CmdError> {
    let done = {
        let mut ctx = EditContext::new(
            &mut canvas.layer_stack,
            &mut canvas.width,
            &mut canvas.height,
            Some(&mut canvas.selection),
        )
        .with_channels(&mut canvas.channels);
        cmd.execute(&mut ctx)
    };
    done.map_err(|e| not_enabled(e.clone(), e))?;
    canvas.record(Box::new(cmd));
    Ok(())
}

fn index(canvas: &Canvas, channel: u32) -> Result<usize, CmdError> {
    let i = channel as usize;
    if i < canvas.channels.alpha.len() {
        Ok(i)
    } else {
        Err(CmdError::new(
            crate::ErrorCode::NotFound,
            format!("no alpha channel {channel}"),
        )
        .vi(format!("Không có kênh alpha {channel}")))
    }
}

/// What a channel command does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelOp {
    /// Keep the selection as a new alpha channel.
    SaveSelection,
    /// A new, empty (black) alpha channel.
    New,
    /// A copy of `channel`, right after it.
    Duplicate,
    Delete,
    /// Give `channel` the name `name`.
    Rename,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChannelParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    pub op: ChannelOp,
    /// The alpha channel (0 the first), for duplicate, delete and rename.
    pub channel: Option<u32>,
    #[schemars(length(min = 1, max = 200))]
    pub name: Option<String>,
}

impl Validate for ChannelParams {
    fn validate(&self) -> Result<(), CmdError> {
        let needs_channel = matches!(
            self.op,
            ChannelOp::Duplicate | ChannelOp::Delete | ChannelOp::Rename
        );
        if needs_channel && self.channel.is_none() {
            return Err(CmdError::bad_param("channel", "which alpha channel"));
        }
        if self.op == ChannelOp::Rename && self.name.as_deref().is_none_or(|n| n.trim().is_empty())
        {
            return Err(CmdError::bad_param("name", "the new name"));
        }
        Ok(())
    }
}

static CHANNEL: CommandMeta = CommandMeta {
    id: "channel.edit",
    label_vi: "Kênh alpha",
    label_en: "Alpha Channels",
    doc: "Alpha channels, as the Channels panel has them: keep the selection as a new channel, \
          add an empty one, duplicate, delete or rename one. One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct EditChannel;

impl Command for EditChannel {
    type Params = ChannelParams;
    type Output = Channels;
    fn meta(&self) -> &'static CommandMeta {
        &CHANNEL
    }
    fn plan(&self, s: &mut Session<'_>, p: ChannelParams) -> Result<Plan<Channels>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let (w, h) = (canvas.width, canvas.height);
        let len = (w as usize)
            .checked_mul(h as usize)
            .ok_or_else(|| CmdError::internal("channel size"))?;
        let fresh = |canvas: &mut Canvas, mask: Vec<u8>| {
            let id = canvas.channels.allocate_alpha_id();
            AlphaChannel {
                id,
                name: ChannelsState::auto_alpha_name(id),
                mask,
                width: w,
                height: h,
                revision: 0,
            }
        };
        match p.op {
            ChannelOp::SaveSelection => {
                if !canvas.selection.active {
                    return Err(not_enabled("nothing is selected", "No selection to save"));
                }
                canvas.selection.refresh_bbox();
                let mut mask = vec![0u8; len];
                for (y, row) in mask.chunks_mut((w as usize).max(1)).enumerate() {
                    for (x, m) in row.iter_mut().enumerate() {
                        let v = canvas.selection.sample(x as u32, y as u32);
                        *m = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
                let channel = fresh(canvas, mask);
                let at = canvas.channels.alpha.len();
                record(
                    canvas,
                    AlphaChannelCommand::insert("Save Selection as Channel", at, channel),
                )?;
                s.notice("Selection saved as channel", "Selection saved as channel");
            }
            ChannelOp::New => {
                let channel = fresh(canvas, vec![0u8; len]);
                let at = canvas.channels.alpha.len();
                record(
                    canvas,
                    AlphaChannelCommand::insert("New Channel", at, channel),
                )?;
            }
            ChannelOp::Duplicate => {
                let i = index(canvas, p.channel.unwrap_or_default())?;
                let mut copy = canvas
                    .channels
                    .alpha
                    .get(i)
                    .cloned()
                    .ok_or_else(|| CmdError::internal("channel"))?;
                copy.name = format!("{} copy", copy.name);
                copy.id = canvas.channels.allocate_alpha_id();
                copy.revision = 0;
                record(
                    canvas,
                    AlphaChannelCommand::insert("Duplicate Channel", i.saturating_add(1), copy),
                )?;
            }
            ChannelOp::Delete => {
                let i = index(canvas, p.channel.unwrap_or_default())?;
                record(canvas, AlphaChannelCommand::remove("Delete Channel", i))?;
            }
            ChannelOp::Rename => {
                let i = index(canvas, p.channel.unwrap_or_default())?;
                let name = p.name.unwrap_or_default().trim().to_string();
                record(
                    canvas,
                    AlphaChannelCommand::rename("Rename Channel", i, name),
                )?;
            }
        }
        channels(s, doc).map(Plan::Done)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LoadParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// The alpha channel (0 the first).
    pub channel: u32,
    #[serde(default)]
    pub mode: SelectMode,
}
impl Validate for LoadParams {}

static LOAD: CommandMeta = CommandMeta {
    id: "select.from_channel",
    label_vi: "Chọn theo kênh alpha",
    label_en: "Load Channel as Selection",
    doc: "Make an alpha channel the selection, or add it to, take it from or intersect it with \
          the selection (the Channels panel's Load). One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct LoadChannel;

impl Command for LoadChannel {
    type Params = LoadParams;
    type Output = Selected;
    fn meta(&self) -> &'static CommandMeta {
        &LOAD
    }
    fn plan(&self, s: &mut Session<'_>, p: LoadParams) -> Result<Plan<Selected>, CmdError> {
        use iai_doc::selection::MaskCombine;
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let i = index(canvas, p.channel)?;
        let (mask, w, h) = {
            let ch = canvas
                .channels
                .alpha
                .get(i)
                .ok_or_else(|| CmdError::internal("channel"))?;
            (ch.mask.clone(), ch.width, ch.height)
        };
        let mode = match p.mode {
            SelectMode::Replace => MaskCombine::Replace,
            SelectMode::Add => MaskCombine::Add,
            SelectMode::Subtract => MaskCombine::Subtract,
            SelectMode::Intersect => MaskCombine::Intersect,
        };
        let mut cmd = SelectionCommand::capture_before("Load Channel", &canvas.selection);
        canvas.selection.combine_with_mask(&mask, w, h, mode);
        cmd.capture_after(&canvas.selection);
        canvas.record(Box::new(cmd));
        selected(s, doc).map(Plan::Done)
    }
}
