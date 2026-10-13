//! Per-document colour-channel state (Channels panel): which channels tools
//! write to, which channel the canvas displays, and saved alpha channels
//! (stored selections). Only alpha channels are serialized into .iai —
//! `view`/`write_mask` are session state and reset on load, like PTS.

/// One saved alpha channel: a canvas-size grayscale mask (e.g. a stored
/// selection). `revision` bumps on every pixel edit so the GPU plane texture
/// and the panel thumbnail know when to refresh.
#[derive(Debug, Clone, PartialEq)]
pub struct AlphaChannel {
    pub id: u32,
    pub name: String,
    pub mask: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
}

/// What the canvas viewport displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelView {
    #[default]
    Composite,
    /// One colour channel as a grayscale plate: 0=R, 1=G, 2=B (3=K reserved
    /// for CMYK). When several colour channels are write-enabled the view
    /// falls back to `Composite` — a two-channel plate has no representation.
    Single(u8),
    /// A saved alpha channel, by `AlphaChannel::id` (stable across deletes).
    Alpha(u32),
}

/// An in-progress Brush/Eraser stroke into an alpha channel (view=Alpha).
/// Captured on the first dab; `Canvas::end_stroke` crops `before`/current to
/// the dab bbox and pushes an `AlphaPlanePaintCommand`.
#[derive(Debug, Clone)]
pub struct PendingAlphaStroke {
    pub alpha_id: u32,
    /// Full mask at stroke start.
    pub before: Vec<u8>,
    /// Union of dab rects, exclusive max (x0, y0, x1, y1); None until a dab lands.
    pub bbox: Option<(u32, u32, u32, u32)>,
}

/// Channel state for one document.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelsState {
    /// Which colour channels tools write to: R, G, B + a fourth slot reserved
    /// for CMYK's K plate (Phase 3). All-true = normal painting; tool dab
    /// loops must treat that case as the untouched fast path.
    pub write_mask: [bool; 4],
    pub view: ChannelView,
    pub alpha: Vec<AlphaChannel>,
    next_alpha_id: u32,
    /// Panel row selection into `alpha` (highlight + delete target).
    pub active_alpha: Option<usize>,
}

impl Default for ChannelsState {
    fn default() -> Self {
        Self {
            write_mask: [true; 4],
            view: ChannelView::Composite,
            alpha: Vec::new(),
            next_alpha_id: 1,
            active_alpha: None,
        }
    }
}

impl ChannelsState {
    /// True when tools write every channel — the per-channel gate must not
    /// alter the hot paint path in this state.
    #[inline]
    pub fn is_default_write(&self) -> bool {
        self.write_mask == [true; 4]
    }

    /// The write mask tools must apply, or `None` for the normal fast path.
    /// Callers painting into a layer mask ignore this (masks are grayscale,
    /// not channel-gated).
    #[inline]
    pub fn write_gate(&self) -> Option<[bool; 4]> {
        if self.is_default_write() {
            None
        } else {
            Some(self.write_mask)
        }
    }

    /// Composite row clicked: show everything, write everything.
    pub fn select_composite(&mut self) {
        self.view = ChannelView::Composite;
        self.write_mask = [true; 4];
        self.active_alpha = None;
    }

    /// Colour row clicked in an RGB document (0=R, 1=G, 2=B). See
    /// [`Self::select_channel_n`].
    pub fn select_color(&mut self, channel: u8, additive: bool) {
        self.select_channel_n(channel, additive, 3);
    }

    /// Colour/ink row clicked. `count` is the document's channel count (3 for
    /// RGB, 4 for CMYK's C/M/Y/K). `additive` = Ctrl/Shift-click: toggles the
    /// channel in the write set instead of replacing it. The view shows the
    /// single plate when exactly one channel is selected, else the composite.
    /// The 4th `write_mask` slot is the K/4th-ink plate; unused channels beyond
    /// `count` are always cleared so an RGB doc never sets the K bit.
    pub fn select_channel_n(&mut self, channel: u8, additive: bool, count: usize) {
        let count = count.clamp(1, 4);
        let c = (channel as usize).min(count - 1);
        let mut wm = [false; 4];
        if additive {
            // Start from the current colour selection (composite counts as none).
            if self.view != ChannelView::Composite || !self.is_default_write() {
                wm = self.write_mask;
                // The all-true composite state is "no specific selection".
                if self.is_default_write() {
                    wm = [false; 4];
                }
            }
            wm[c] = !wm[c];
            if !wm.iter().any(|&on| on) {
                wm[c] = true; // never leave an empty write set
            }
        } else {
            wm[c] = true;
        }
        // Channels past the document's count can never be enabled.
        for slot in wm.iter_mut().skip(count) {
            *slot = false;
        }
        self.write_mask = wm;
        let selected: Vec<usize> = (0..count).filter(|&i| wm[i]).collect();
        self.view = if selected.len() == 1 {
            ChannelView::Single(selected[0] as u8)
        } else {
            ChannelView::Composite
        };
        self.active_alpha = None;
    }

    /// Alpha row clicked (index into `alpha`).
    pub fn select_alpha(&mut self, index: usize) {
        if let Some(ch) = self.alpha.get(index) {
            self.view = ChannelView::Alpha(ch.id);
            self.active_alpha = Some(index);
        }
    }

    /// Add a new alpha channel and return its index. `name` empty = auto
    /// "Alpha N".
    #[allow(dead_code)] // wired up by the alpha-channel ops (slice 2.4)
    pub fn add_alpha(&mut self, name: String, mask: Vec<u8>, width: u32, height: u32) -> usize {
        let id = self.next_alpha_id;
        self.next_alpha_id += 1;
        let name = if name.trim().is_empty() {
            format!("Alpha {}", id)
        } else {
            name
        };
        self.alpha.push(AlphaChannel {
            id,
            name,
            mask,
            width,
            height,
            revision: 0,
        });
        self.alpha.len() - 1
    }

    /// Allocate a fresh alpha-channel id (for channels built outside
    /// `add_alpha`, e.g. by an undoable insert command).
    pub fn allocate_alpha_id(&mut self) -> u32 {
        let id = self.next_alpha_id;
        self.next_alpha_id += 1;
        id
    }

    /// Default name for a new channel with this id.
    pub fn auto_alpha_name(id: u32) -> String {
        format!("Alpha {}", id)
    }

    /// Rename the channel at `index`, returning the old name. Bumps the
    /// channel revision so cached panel rows/thumbnails refresh.
    pub fn rename_alpha(&mut self, index: usize, name: String) -> Option<String> {
        let ch = self.alpha.get_mut(index)?;
        let old = std::mem::replace(&mut ch.name, name);
        ch.revision += 1;
        Some(old)
    }

    /// Re-insert a previously removed channel at `index` (undo of delete).
    pub fn insert_alpha(&mut self, index: usize, channel: AlphaChannel) {
        let index = index.min(self.alpha.len());
        // Keep ids unique if the file was hand-edited or state drifted.
        self.next_alpha_id = self.next_alpha_id.max(channel.id + 1);
        self.alpha.insert(index, channel);
    }

    pub fn remove_alpha(&mut self, index: usize) -> Option<AlphaChannel> {
        if index >= self.alpha.len() {
            return None;
        }
        let ch = self.alpha.remove(index);
        if self.view == ChannelView::Alpha(ch.id) {
            self.select_composite();
        }
        match self.active_alpha {
            Some(a) if a == index => self.active_alpha = None,
            Some(a) if a > index => self.active_alpha = Some(a - 1),
            _ => {}
        }
        Some(ch)
    }

    pub fn alpha_index_of(&self, id: u32) -> Option<usize> {
        self.alpha.iter().position(|c| c.id == id)
    }

    /// The alpha channel currently shown by the view, if any.
    #[allow(dead_code)] // wired up by the alpha plane view (slice 2.5)
    pub fn viewed_alpha(&self) -> Option<&AlphaChannel> {
        match self.view {
            ChannelView::Alpha(id) => self.alpha.iter().find(|c| c.id == id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_row_selection_couples_view_and_write_mask() {
        let mut ch = ChannelsState::default();
        ch.select_color(0, false);
        assert_eq!(ch.write_mask, [true, false, false, false]);
        assert_eq!(ch.view, ChannelView::Single(0));

        // Ctrl-click G: two channels selected -> composite view.
        ch.select_color(1, true);
        assert_eq!(ch.write_mask, [true, true, false, false]);
        assert_eq!(ch.view, ChannelView::Composite);

        // Ctrl-click R again: back to G alone.
        ch.select_color(0, true);
        assert_eq!(ch.write_mask, [false, true, false, false]);
        assert_eq!(ch.view, ChannelView::Single(1));

        // Toggling the last channel off is refused.
        ch.select_color(1, true);
        assert_eq!(ch.write_mask, [false, true, false, false]);

        ch.select_composite();
        assert!(ch.is_default_write());
        assert_eq!(ch.view, ChannelView::Composite);
    }

    #[test]
    fn cmyk_selects_four_channels_including_k_plate() {
        let mut ch = ChannelsState::default();
        // K plate (slot 3) is reachable only with count = 4.
        ch.select_channel_n(3, false, 4);
        assert_eq!(ch.write_mask, [false, false, false, true]);
        assert_eq!(ch.view, ChannelView::Single(3));

        // Ctrl-click Cyan adds it: two plates → composite view.
        ch.select_channel_n(0, true, 4);
        assert_eq!(ch.write_mask, [true, false, false, true]);
        assert_eq!(ch.view, ChannelView::Composite);

        // An RGB doc (count = 3) can never light the K/4th slot.
        let mut rgb = ChannelsState::default();
        rgb.select_channel_n(3, false, 3);
        assert_eq!(rgb.write_mask, [false, false, true, false]); // clamped to Blue
        assert_eq!(rgb.view, ChannelView::Single(2));
    }

    #[test]
    fn additive_click_from_composite_selects_only_that_channel() {
        let mut ch = ChannelsState::default();
        ch.select_color(2, true);
        assert_eq!(ch.write_mask, [false, false, true, false]);
        assert_eq!(ch.view, ChannelView::Single(2));
    }

    #[test]
    fn alpha_channels_auto_name_and_keep_ids_stable() {
        let mut ch = ChannelsState::default();
        let a = ch.add_alpha(String::new(), vec![0; 4], 2, 2);
        let b = ch.add_alpha(String::new(), vec![255; 4], 2, 2);
        assert_eq!(ch.alpha[a].name, "Alpha 1");
        assert_eq!(ch.alpha[b].name, "Alpha 2");

        ch.select_alpha(a);
        assert_eq!(ch.view, ChannelView::Alpha(ch.alpha[a].id));
        assert_eq!(ch.active_alpha, Some(a));

        // Deleting the viewed channel falls back to composite; ids of the
        // remaining channels are untouched.
        let removed = ch.remove_alpha(a).unwrap();
        assert_eq!(removed.name, "Alpha 1");
        assert_eq!(ch.view, ChannelView::Composite);
        assert_eq!(ch.active_alpha, None);
        assert_eq!(ch.alpha[0].name, "Alpha 2");

        // Undo of the delete restores the same id and bumps next_alpha_id past it.
        ch.insert_alpha(0, removed);
        assert_eq!(ch.alpha_index_of(1), Some(0));
        let c = ch.add_alpha(String::new(), vec![0; 4], 2, 2);
        assert_eq!(ch.alpha[c].name, "Alpha 3");
    }
}
