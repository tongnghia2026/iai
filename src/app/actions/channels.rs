//! Channels panel actions: row selection (view + write mask coupling) and
//! the alpha-channel operations (save/load selection, new, rename, delete,
//! duplicate). Alpha edits go through `AlphaChannelCommand` so they are
//! undoable; load-as-selection pushes a `SelectionCommand`.

use crate::app::state::App;
use crate::core::channels::{AlphaChannel, ChannelView};
use crate::core::command::{AlphaChannelCommand, Command, EditContext, SelectionCommand};
use crate::ui::UiActions;

impl App {
    pub(super) fn handle_channel_actions(&mut self, actions: &mut UiActions) {
        let mut changed = false;

        if let Some((view, additive)) = actions.channels.select_channel_row.take() {
            let cmyk = self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .is_cmyk();
            let channels = &mut self.docs.documents[self.docs.active_doc_idx]
                .canvas
                .channels;
            match view {
                ChannelView::Composite => channels.select_composite(),
                ChannelView::Single(c) if cmyk => channels.select_channel_n(c, additive, 4),
                ChannelView::Single(c) => channels.select_color(c, additive),
                ChannelView::Alpha(id) => {
                    if let Some(idx) = channels.alpha_index_of(id) {
                        channels.select_alpha(idx);
                    }
                }
            }
            // Refresh the blit uniforms and, for saved alpha rows, upload the
            // viewed mask into the display plane.
            self.refresh_channel_view_display();
            changed = true;
        }

        if actions.channels.save_selection_as_channel {
            changed |= self.save_selection_as_channel();
        }
        if actions.channels.new_alpha_channel {
            changed |= self.new_empty_alpha_channel();
        }
        if let Some(idx) = actions.channels.duplicate_alpha_channel.take() {
            changed |= self.duplicate_alpha_channel(idx);
        }
        if let Some(idx) = actions.channels.delete_alpha_channel.take() {
            changed |= self.push_alpha_command(AlphaChannelCommand::remove("Delete Channel", idx));
        }
        if let Some((idx, name)) = actions.channels.rename_alpha_channel.take() {
            let name = name.trim().to_string();
            if !name.is_empty() {
                changed |= self.push_alpha_command(AlphaChannelCommand::rename(
                    "Rename Channel",
                    idx,
                    name,
                ));
            }
        }
        if let Some((idx, mode)) = actions.channels.load_channel_as_selection.take() {
            changed |= self.load_channel_as_selection(idx, mode);
        }

        if changed {
            self.refresh_channel_view_display();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }

    /// Execute an alpha-channel command against the active canvas and push it
    /// onto the history. Returns true when the command applied.
    fn push_alpha_command(&mut self, mut cmd: AlphaChannelCommand) -> bool {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let applied = {
            let mut ctx = EditContext::new(
                &mut canvas.layer_stack,
                &mut canvas.width,
                &mut canvas.height,
                Some(&mut canvas.selection),
            )
            .with_channels(&mut canvas.channels);
            cmd.execute(&mut ctx).is_ok()
        };
        if applied {
            canvas.record(Box::new(cmd));
        }
        applied
    }

    /// "Save Selection as Channel": rasterise the active selection into a new
    /// alpha channel (canvas space, offset resolved by `Selection::sample`).
    fn save_selection_as_channel(&mut self) -> bool {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        if !canvas.selection.active {
            self.shell.status_msg = "No selection to save".to_string();
            return false;
        }
        canvas.selection.refresh_bbox();
        let (w, h) = (canvas.width, canvas.height);
        let Some(len) = (w as usize).checked_mul(h as usize) else {
            return false;
        };
        let mut mask = vec![0u8; len];
        for y in 0..h {
            let row = (y as usize) * (w as usize);
            for x in 0..w {
                mask[row + x as usize] = (canvas.selection.sample(x, y) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
        let id = canvas.channels.allocate_alpha_id();
        let channel = AlphaChannel {
            id,
            name: crate::core::channels::ChannelsState::auto_alpha_name(id),
            mask,
            width: w,
            height: h,
            revision: 0,
        };
        let index = canvas.channels.alpha.len();
        let ok = self.push_alpha_command(AlphaChannelCommand::insert(
            "Save Selection as Channel",
            index,
            channel,
        ));
        if ok {
            self.shell.status_msg = "Selection saved as channel".to_string();
        }
        ok
    }

    /// "New Channel": an empty (black) alpha channel.
    fn new_empty_alpha_channel(&mut self) -> bool {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let (w, h) = (canvas.width, canvas.height);
        let Some(len) = (w as usize).checked_mul(h as usize) else {
            return false;
        };
        let id = canvas.channels.allocate_alpha_id();
        let channel = AlphaChannel {
            id,
            name: crate::core::channels::ChannelsState::auto_alpha_name(id),
            mask: vec![0u8; len],
            width: w,
            height: h,
            revision: 0,
        };
        let index = canvas.channels.alpha.len();
        self.push_alpha_command(AlphaChannelCommand::insert("New Channel", index, channel))
    }

    fn duplicate_alpha_channel(&mut self, index: usize) -> bool {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(src) = canvas.channels.alpha.get(index) else {
            return false;
        };
        let mut copy = src.clone();
        copy.name = format!("{} copy", src.name);
        copy.id = canvas.channels.allocate_alpha_id();
        copy.revision = 0;
        self.push_alpha_command(AlphaChannelCommand::insert(
            "Duplicate Channel",
            index + 1,
            copy,
        ))
    }

    /// "Load Channel as Selection" with a combine mode; undoable via
    /// SelectionCommand (RLE-compressed masks).
    fn load_channel_as_selection(
        &mut self,
        index: usize,
        mode: crate::core::selection::MaskCombine,
    ) -> bool {
        let canvas = &mut self.docs.documents[self.docs.active_doc_idx].canvas;
        let Some(ch) = canvas.channels.alpha.get(index) else {
            return false;
        };
        let (mask, w, h) = (ch.mask.clone(), ch.width, ch.height);
        let mut cmd = SelectionCommand::capture_before("Load Channel", &canvas.selection);
        canvas.selection.combine_with_mask(&mask, w, h, mode);
        cmd.capture_after(&canvas.selection);
        canvas.record(Box::new(cmd));
        self.upload_selection_mask();
        true
    }
}
