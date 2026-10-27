//! Channels panel actions: row selection (view + write mask coupling). The
//! alpha-channel operations are commands (`channel.edit`,
//! `select.from_channel`).

use crate::app::state::App;
use crate::core::channels::ChannelView;
use crate::ui::UiActions;

impl App {
    pub(super) fn handle_channel_actions(&mut self, actions: &mut UiActions) {
        let mut changed = false;

        if let Some((view, additive)) = actions.channels.select_channel_row.take() {
            let cmyk = self.docs.active().canvas.is_cmyk();
            let channels = &mut self.docs.active_mut().canvas.channels;
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

        if changed {
            self.refresh_channel_view_display();
            if let Some(w) = &self.win.window {
                w.request_redraw();
            }
        }
    }
}
