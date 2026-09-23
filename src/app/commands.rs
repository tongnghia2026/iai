//! Keymap engine: the re-bindable commands and their key chords.
//!
//! This is the single source of truth for the shortcut strings shown in the
//! menu bar, the Help ▸ Keyboard Shortcuts list and the Preferences ▸ Shortcuts
//! page — so a binding is described in exactly one place instead of being typed
//! out at each of them.
//!
//! Scope is the "pragmatic" set the owner approved: the tool-select keys and the
//! menu-command keys. Context-sensitive keys (Enter/Esc/Space, arrows, `[` `]`,
//! Delete, proof/merge/group/convert, colour swap/reset…) are deliberately NOT
//! here — they stay hard-wired in `keyboard.rs`.
//!
//! Phase 2 wires only the *display* of these bindings to this module. Routing
//! key *dispatch* through a mutable keymap, plus the rebinding editor and reset,
//! is Phase 3; the `KeyChord`/`KeyMap`/`KeyName` types below are built now so
//! that step is additive.

use serde::{Deserialize, Serialize};

/// Every re-bindable action. The string `id` (see [`Command::id`]) is the stable
/// key used when a custom keymap is saved, so variants may be reordered freely
/// but an `id` must never change once shipped.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Command {
    // Tools
    ToolMove,
    ToolMarquee,
    ToolLasso,
    ToolQuickSelect,
    ToolCrop,
    ToolEyedropper,
    ToolBrush,
    ToolClone,
    ToolEraser,
    ToolFill,
    ToolDodge,
    ToolPen,
    ToolNode,
    ToolText,
    ToolShape,
    ToolRepair,
    ToolZoom,
    ToolHand,
    // File
    FileNew,
    FileOpen,
    FileSave,
    FileSaveAs,
    FileClose,
    FilePrint,
    Preferences,
    // Edit
    EditUndo,
    EditRedo,
    EditCut,
    EditCopy,
    EditPaste,
    SelectAll,
    FreeTransform,
    LayerViaCopy,
    // Image / adjustments
    Levels,
    AutoLevels,
    Curves,
    ColorBalance,
    HueSaturation,
    Desaturate,
    Invert,
    // View
    ToggleRulers,
    FitScreen,
    ZoomActual,
    // Window
    OpenDevelop,
}

/// Coarse grouping used to lay the shortcut lists out in sections.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandGroup {
    Tools,
    File,
    Edit,
    Image,
    View,
    Window,
}

impl CommandGroup {
    pub fn title(self) -> &'static str {
        match self {
            CommandGroup::Tools => "Tools",
            CommandGroup::File => "File",
            CommandGroup::Edit => "Edit",
            CommandGroup::Image => "Image & colour",
            CommandGroup::View => "View",
            CommandGroup::Window => "Window",
        }
    }

    /// The order sections are shown in the lists.
    pub fn all() -> [CommandGroup; 6] {
        [
            CommandGroup::Tools,
            CommandGroup::File,
            CommandGroup::Edit,
            CommandGroup::Image,
            CommandGroup::View,
            CommandGroup::Window,
        ]
    }
}

/// A physical key that a chord can name. Only the keys the re-bindable commands
/// actually use are listed. Kept separate from winit's `KeyCode` so this module
/// stays dependency-free and serialisable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
pub enum KeyName {
    A,
    B,
    C,
    E,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Z,
    Digit0,
    Digit1,
    Comma,
}

impl KeyName {
    /// How the key reads in a shortcut label (e.g. `S`, `0`, `,`).
    pub fn label(self) -> &'static str {
        match self {
            KeyName::A => "A",
            KeyName::B => "B",
            KeyName::C => "C",
            KeyName::E => "E",
            KeyName::G => "G",
            KeyName::H => "H",
            KeyName::I => "I",
            KeyName::J => "J",
            KeyName::K => "K",
            KeyName::L => "L",
            KeyName::M => "M",
            KeyName::N => "N",
            KeyName::O => "O",
            KeyName::P => "P",
            KeyName::R => "R",
            KeyName::S => "S",
            KeyName::T => "T",
            KeyName::U => "U",
            KeyName::V => "V",
            KeyName::W => "W",
            KeyName::X => "X",
            KeyName::Z => "Z",
            KeyName::Digit0 => "0",
            KeyName::Digit1 => "1",
            KeyName::Comma => ",",
        }
    }
}

/// A key combination: modifiers plus one key.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
pub struct KeyChord {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: KeyName,
}

impl KeyChord {
    pub const fn new(ctrl: bool, shift: bool, alt: bool, key: KeyName) -> Self {
        Self {
            ctrl,
            shift,
            alt,
            key,
        }
    }

    /// Plain key, no modifiers.
    pub const fn plain(key: KeyName) -> Self {
        Self::new(false, false, false, key)
    }

    /// Ctrl + key.
    pub const fn ctrl(key: KeyName) -> Self {
        Self::new(true, false, false, key)
    }

    /// Ctrl + Shift + key.
    pub const fn ctrl_shift(key: KeyName) -> Self {
        Self::new(true, true, false, key)
    }

    /// Human-readable label, e.g. `Ctrl+Shift+M`. Order is Ctrl, Shift, Alt, key.
    pub fn label(self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        out.push_str(self.key.label());
        out
    }
}

/// One default binding row: the command, its default chord, its group and the
/// name shown to the user. This array is the single source the whole engine and
/// the shortcut lists read from.
const TABLE: &[(Command, KeyChord, CommandGroup, &str)] = &[
    // Tools
    (
        Command::ToolMove,
        KeyChord::plain(KeyName::V),
        CommandGroup::Tools,
        "Move",
    ),
    (
        Command::ToolMarquee,
        KeyChord::plain(KeyName::M),
        CommandGroup::Tools,
        "Marquee",
    ),
    (
        Command::ToolLasso,
        KeyChord::plain(KeyName::L),
        CommandGroup::Tools,
        "Lasso",
    ),
    (
        Command::ToolQuickSelect,
        KeyChord::plain(KeyName::W),
        CommandGroup::Tools,
        "Smart Select",
    ),
    (
        Command::ToolCrop,
        KeyChord::plain(KeyName::C),
        CommandGroup::Tools,
        "Crop",
    ),
    (
        Command::ToolEyedropper,
        KeyChord::plain(KeyName::I),
        CommandGroup::Tools,
        "Eyedropper",
    ),
    (
        Command::ToolBrush,
        KeyChord::plain(KeyName::B),
        CommandGroup::Tools,
        "Brush",
    ),
    (
        Command::ToolClone,
        KeyChord::plain(KeyName::S),
        CommandGroup::Tools,
        "Clone Stamp",
    ),
    (
        Command::ToolEraser,
        KeyChord::plain(KeyName::E),
        CommandGroup::Tools,
        "Eraser",
    ),
    (
        Command::ToolFill,
        KeyChord::plain(KeyName::G),
        CommandGroup::Tools,
        "Fill / Gradient",
    ),
    (
        Command::ToolDodge,
        KeyChord::plain(KeyName::O),
        CommandGroup::Tools,
        "Dodge / Burn",
    ),
    (
        Command::ToolPen,
        KeyChord::plain(KeyName::P),
        CommandGroup::Tools,
        "Pen",
    ),
    (
        Command::ToolNode,
        KeyChord::plain(KeyName::A),
        CommandGroup::Tools,
        "Direct Select (Node)",
    ),
    (
        Command::ToolText,
        KeyChord::plain(KeyName::T),
        CommandGroup::Tools,
        "Type",
    ),
    (
        Command::ToolShape,
        KeyChord::plain(KeyName::U),
        CommandGroup::Tools,
        "Shapes",
    ),
    (
        Command::ToolRepair,
        KeyChord::plain(KeyName::J),
        CommandGroup::Tools,
        "Repair / Patch",
    ),
    (
        Command::ToolZoom,
        KeyChord::plain(KeyName::Z),
        CommandGroup::Tools,
        "Zoom",
    ),
    (
        Command::ToolHand,
        KeyChord::plain(KeyName::H),
        CommandGroup::Tools,
        "Hand",
    ),
    // File
    (
        Command::FileNew,
        KeyChord::ctrl(KeyName::N),
        CommandGroup::File,
        "New",
    ),
    (
        Command::FileOpen,
        KeyChord::ctrl(KeyName::O),
        CommandGroup::File,
        "Open",
    ),
    (
        Command::FileSave,
        KeyChord::ctrl(KeyName::S),
        CommandGroup::File,
        "Save",
    ),
    (
        Command::FileSaveAs,
        KeyChord::ctrl_shift(KeyName::S),
        CommandGroup::File,
        "Save As",
    ),
    (
        Command::FileClose,
        KeyChord::ctrl(KeyName::W),
        CommandGroup::File,
        "Close",
    ),
    (
        Command::FilePrint,
        KeyChord::ctrl(KeyName::P),
        CommandGroup::File,
        "Print",
    ),
    (
        Command::Preferences,
        KeyChord::ctrl(KeyName::K),
        CommandGroup::File,
        "Preferences",
    ),
    // Edit
    (
        Command::EditUndo,
        KeyChord::ctrl(KeyName::Z),
        CommandGroup::Edit,
        "Undo",
    ),
    (
        Command::EditRedo,
        KeyChord::ctrl_shift(KeyName::Z),
        CommandGroup::Edit,
        "Redo",
    ),
    (
        Command::EditCut,
        KeyChord::ctrl(KeyName::X),
        CommandGroup::Edit,
        "Cut",
    ),
    (
        Command::EditCopy,
        KeyChord::ctrl(KeyName::C),
        CommandGroup::Edit,
        "Copy",
    ),
    (
        Command::EditPaste,
        KeyChord::ctrl(KeyName::V),
        CommandGroup::Edit,
        "Paste",
    ),
    (
        Command::SelectAll,
        KeyChord::ctrl(KeyName::A),
        CommandGroup::Edit,
        "Select All",
    ),
    (
        Command::FreeTransform,
        KeyChord::ctrl(KeyName::T),
        CommandGroup::Edit,
        "Free Transform",
    ),
    (
        Command::LayerViaCopy,
        KeyChord::ctrl(KeyName::J),
        CommandGroup::Edit,
        "Layer via Copy",
    ),
    // Image / adjustments
    (
        Command::Levels,
        KeyChord::ctrl(KeyName::L),
        CommandGroup::Image,
        "Levels",
    ),
    (
        Command::AutoLevels,
        KeyChord::ctrl_shift(KeyName::L),
        CommandGroup::Image,
        "Auto Levels",
    ),
    (
        Command::Curves,
        KeyChord::ctrl(KeyName::M),
        CommandGroup::Image,
        "Curves",
    ),
    (
        Command::ColorBalance,
        KeyChord::ctrl(KeyName::B),
        CommandGroup::Image,
        "Color Balance",
    ),
    (
        Command::HueSaturation,
        KeyChord::ctrl(KeyName::U),
        CommandGroup::Image,
        "Hue/Saturation",
    ),
    (
        Command::Desaturate,
        KeyChord::ctrl_shift(KeyName::U),
        CommandGroup::Image,
        "Desaturate",
    ),
    (
        Command::Invert,
        KeyChord::ctrl(KeyName::I),
        CommandGroup::Image,
        "Invert",
    ),
    // View
    (
        Command::ToggleRulers,
        KeyChord::ctrl(KeyName::R),
        CommandGroup::View,
        "Rulers",
    ),
    (
        Command::FitScreen,
        KeyChord::ctrl(KeyName::Digit0),
        CommandGroup::View,
        "Fit to window",
    ),
    (
        Command::ZoomActual,
        KeyChord::ctrl(KeyName::Digit1),
        CommandGroup::View,
        "100%",
    ),
    // Window
    (
        Command::OpenDevelop,
        KeyChord::ctrl_shift(KeyName::A),
        CommandGroup::Window,
        "Develop",
    ),
];

impl Command {
    /// Stable identifier used when a custom keymap is persisted (Phase 3).
    pub fn id(self) -> &'static str {
        match self {
            Command::ToolMove => "tool.move",
            Command::ToolMarquee => "tool.marquee",
            Command::ToolLasso => "tool.lasso",
            Command::ToolQuickSelect => "tool.quick_select",
            Command::ToolCrop => "tool.crop",
            Command::ToolEyedropper => "tool.eyedropper",
            Command::ToolBrush => "tool.brush",
            Command::ToolClone => "tool.clone",
            Command::ToolEraser => "tool.eraser",
            Command::ToolFill => "tool.fill",
            Command::ToolDodge => "tool.dodge",
            Command::ToolPen => "tool.pen",
            Command::ToolNode => "tool.node",
            Command::ToolText => "tool.text",
            Command::ToolShape => "tool.shape",
            Command::ToolRepair => "tool.repair",
            Command::ToolZoom => "tool.zoom",
            Command::ToolHand => "tool.hand",
            Command::FileNew => "file.new",
            Command::FileOpen => "file.open",
            Command::FileSave => "file.save",
            Command::FileSaveAs => "file.save_as",
            Command::FileClose => "file.close",
            Command::FilePrint => "file.print",
            Command::Preferences => "app.preferences",
            Command::EditUndo => "edit.undo",
            Command::EditRedo => "edit.redo",
            Command::EditCut => "edit.cut",
            Command::EditCopy => "edit.copy",
            Command::EditPaste => "edit.paste",
            Command::SelectAll => "select.all",
            Command::FreeTransform => "edit.free_transform",
            Command::LayerViaCopy => "layer.via_copy",
            Command::Levels => "image.levels",
            Command::AutoLevels => "image.auto_levels",
            Command::Curves => "image.curves",
            Command::ColorBalance => "image.color_balance",
            Command::HueSaturation => "image.hue_saturation",
            Command::Desaturate => "image.desaturate",
            Command::Invert => "image.invert",
            Command::ToggleRulers => "view.rulers",
            Command::FitScreen => "view.fit_screen",
            Command::ZoomActual => "view.zoom_actual",
            Command::OpenDevelop => "window.develop",
        }
    }

    fn row(self) -> &'static (Command, KeyChord, CommandGroup, &'static str) {
        TABLE
            .iter()
            .find(|(cmd, _, _, _)| *cmd == self)
            .expect("every Command has a TABLE row")
    }

    /// The name shown to the user.
    pub fn display_name(self) -> &'static str {
        self.row().3
    }

    pub fn group(self) -> CommandGroup {
        self.row().2
    }

    /// The built-in default chord for this command.
    pub fn default_chord(self) -> KeyChord {
        self.row().1
    }

    /// The default shortcut label, e.g. `Ctrl+S`.
    pub fn default_label(self) -> String {
        self.default_chord().label()
    }

    /// Every command, in table (grouped) order.
    pub fn all() -> impl Iterator<Item = Command> {
        TABLE.iter().map(|(cmd, _, _, _)| *cmd)
    }

    /// The commands in one group, in table order.
    pub fn in_group(group: CommandGroup) -> impl Iterator<Item = Command> {
        TABLE
            .iter()
            .filter(move |(_, _, g, _)| *g == group)
            .map(|(cmd, _, _, _)| *cmd)
    }
}

/// The active bindings. In Phase 2 this only ever holds the defaults; Phase 3
/// lets the user override rows and persists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyMap {
    bindings: Vec<(Command, KeyChord)>,
}

impl Default for KeyMap {
    fn default() -> Self {
        Self {
            bindings: TABLE
                .iter()
                .map(|(cmd, chord, _, _)| (*cmd, *chord))
                .collect(),
        }
    }
}

impl KeyMap {
    /// The chord currently bound to `cmd`, if any.
    pub fn chord_for(&self, cmd: Command) -> Option<KeyChord> {
        self.bindings
            .iter()
            .find(|(c, _)| *c == cmd)
            .map(|(_, chord)| *chord)
    }

    /// The command a chord triggers, if any.
    pub fn command_for(&self, chord: KeyChord) -> Option<Command> {
        self.bindings
            .iter()
            .find(|(_, c)| *c == chord)
            .map(|(cmd, _)| *cmd)
    }

    /// Label for `cmd`'s current binding (empty if somehow unbound).
    pub fn label_for(&self, cmd: Command) -> String {
        self.chord_for(cmd).map(KeyChord::label).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_command_appears_exactly_once() {
        let mut seen = HashSet::new();
        for cmd in Command::all() {
            assert!(seen.insert(cmd), "duplicate command in TABLE: {cmd:?}");
        }
    }

    #[test]
    fn default_chords_are_unique() {
        let mut seen = HashSet::new();
        for cmd in Command::all() {
            let chord = cmd.default_chord();
            assert!(
                seen.insert(chord),
                "two commands share the default chord {}: second is {:?}",
                chord.label(),
                cmd
            );
        }
    }

    #[test]
    fn command_ids_are_unique_and_stable_shape() {
        let mut seen = HashSet::new();
        for cmd in Command::all() {
            let id = cmd.id();
            assert!(seen.insert(id), "duplicate id {id}");
            assert!(id.contains('.'), "id {id} should be namespaced");
        }
    }

    #[test]
    fn labels_match_expected() {
        assert_eq!(Command::FileSave.default_label(), "Ctrl+S");
        assert_eq!(Command::FileSaveAs.default_label(), "Ctrl+Shift+S");
        assert_eq!(Command::ToolBrush.default_label(), "B");
        assert_eq!(Command::FitScreen.default_label(), "Ctrl+0");
        assert_eq!(Command::Preferences.default_label(), "Ctrl+K");
    }

    #[test]
    fn keymap_default_round_trips_commands_and_chords() {
        let km = KeyMap::default();
        for cmd in Command::all() {
            let chord = km.chord_for(cmd).expect("bound");
            assert_eq!(chord, cmd.default_chord());
            assert_eq!(km.command_for(chord), Some(cmd));
        }
    }
}
