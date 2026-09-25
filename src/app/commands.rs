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
//! The user's changes are stored as *overrides* only (command id → chord label,
//! `""` = no key) in `prefs.json`; [`KeyMap::from_overrides`] rebuilds the full
//! map and repairs anything unreadable, reserved or clashing, so a bad file can
//! never leave the app without working shortcuts. A command whose binding is
//! still the built-in default keeps running through the original key arms in
//! `keyboard.rs`, so an untouched keymap behaves exactly as before.

use std::collections::BTreeMap;

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

/// A physical key that a chord can name: letters, digits, F-keys and a few
/// punctuation keys. Context keys (Space, Enter, Esc, arrows, Delete, brackets,
/// `+`/`-`) are absent on purpose — they stay hard-wired. Kept separate from
/// winit's `KeyCode` so this module stays dependency-free and serialisable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
pub enum KeyName {
    A,
    B,
    C,
    D,
    E,
    F,
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
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Comma,
    Period,
    Slash,
    Semicolon,
    Quote,
    Backquote,
    Backslash,
}

impl KeyName {
    /// Every nameable key, in display order.
    pub const ALL: [KeyName; 55] = [
        KeyName::A,
        KeyName::B,
        KeyName::C,
        KeyName::D,
        KeyName::E,
        KeyName::F,
        KeyName::G,
        KeyName::H,
        KeyName::I,
        KeyName::J,
        KeyName::K,
        KeyName::L,
        KeyName::M,
        KeyName::N,
        KeyName::O,
        KeyName::P,
        KeyName::Q,
        KeyName::R,
        KeyName::S,
        KeyName::T,
        KeyName::U,
        KeyName::V,
        KeyName::W,
        KeyName::X,
        KeyName::Y,
        KeyName::Z,
        KeyName::Digit0,
        KeyName::Digit1,
        KeyName::Digit2,
        KeyName::Digit3,
        KeyName::Digit4,
        KeyName::Digit5,
        KeyName::Digit6,
        KeyName::Digit7,
        KeyName::Digit8,
        KeyName::Digit9,
        KeyName::F1,
        KeyName::F2,
        KeyName::F3,
        KeyName::F4,
        KeyName::F5,
        KeyName::F6,
        KeyName::F7,
        KeyName::F8,
        KeyName::F9,
        KeyName::F10,
        KeyName::F11,
        KeyName::F12,
        KeyName::Comma,
        KeyName::Period,
        KeyName::Slash,
        KeyName::Semicolon,
        KeyName::Quote,
        KeyName::Backquote,
        KeyName::Backslash,
    ];

    /// How the key reads in a shortcut label (e.g. `S`, `0`, `F5`, `,`).
    pub fn label(self) -> &'static str {
        match self {
            KeyName::A => "A",
            KeyName::B => "B",
            KeyName::C => "C",
            KeyName::D => "D",
            KeyName::E => "E",
            KeyName::F => "F",
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
            KeyName::Q => "Q",
            KeyName::R => "R",
            KeyName::S => "S",
            KeyName::T => "T",
            KeyName::U => "U",
            KeyName::V => "V",
            KeyName::W => "W",
            KeyName::X => "X",
            KeyName::Y => "Y",
            KeyName::Z => "Z",
            KeyName::Digit0 => "0",
            KeyName::Digit1 => "1",
            KeyName::Digit2 => "2",
            KeyName::Digit3 => "3",
            KeyName::Digit4 => "4",
            KeyName::Digit5 => "5",
            KeyName::Digit6 => "6",
            KeyName::Digit7 => "7",
            KeyName::Digit8 => "8",
            KeyName::Digit9 => "9",
            KeyName::F1 => "F1",
            KeyName::F2 => "F2",
            KeyName::F3 => "F3",
            KeyName::F4 => "F4",
            KeyName::F5 => "F5",
            KeyName::F6 => "F6",
            KeyName::F7 => "F7",
            KeyName::F8 => "F8",
            KeyName::F9 => "F9",
            KeyName::F10 => "F10",
            KeyName::F11 => "F11",
            KeyName::F12 => "F12",
            KeyName::Comma => ",",
            KeyName::Period => ".",
            KeyName::Slash => "/",
            KeyName::Semicolon => ";",
            KeyName::Quote => "'",
            KeyName::Backquote => "`",
            KeyName::Backslash => "\\",
        }
    }

    /// Inverse of [`Self::label`]; letters and F-keys ignore case.
    pub fn from_label(text: &str) -> Option<KeyName> {
        KeyName::ALL
            .into_iter()
            .find(|key| key.label().eq_ignore_ascii_case(text))
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

    /// Inverse of [`Self::label`]: `"Ctrl+Shift+K"`, `"B"`, `"Alt+F5"`.
    /// Modifier names ignore case; `None` for anything unreadable.
    pub fn parse(text: &str) -> Option<KeyChord> {
        let mut parts: Vec<&str> = text.trim().split('+').map(str::trim).collect();
        let key = KeyName::from_label(parts.pop()?)?;
        let mut chord = KeyChord::plain(key);
        for part in parts {
            let flag = match part.to_ascii_lowercase().as_str() {
                "ctrl" => &mut chord.ctrl,
                "shift" => &mut chord.shift,
                "alt" => &mut chord.alt,
                _ => return None,
            };
            if *flag {
                return None;
            }
            *flag = true;
        }
        Some(chord)
    }
}

/// The fixed (non-rebindable) keys, as shown read-only in Help and Preferences.
pub const FIXED_SHORTCUTS: &[(&str, &str)] = &[
    ("X", "Swap colours"),
    ("D", "Reset colours"),
    ("Ctrl+D", "Deselect / Repeat"),
    ("Ctrl+Shift+D", "Repeat"),
    ("Ctrl+G", "Group"),
    ("Ctrl+Shift+G", "Ungroup"),
    ("Ctrl+Alt+G", "Clipping Mask"),
    ("Ctrl+E", "Merge Down"),
    ("Ctrl+Shift+E", "Stamp Visible"),
    ("Ctrl+Q", "Convert to Curves"),
    ("Ctrl+Y", "Proof Colors"),
    ("Ctrl+Shift+Y", "Gamut Warning"),
    ("Ctrl+Shift+X", "Warp"),
    ("Ctrl+Alt+I", "Image Size"),
    ("Ctrl+Alt+R", "Refine Selection"),
    ("Shift+F5", "Smart Fill"),
    ("Shift+F6", "Feather"),
    ("Shift+F7", "Invert Selection"),
    ("Ctrl++ / Ctrl+-", "Zoom in / out"),
    ("[  ]", "Brush size"),
    ("Shift+[  ]", "Brush hardness"),
    ("Space+Drag", "Pan"),
    ("Enter / Esc", "Commit / Cancel"),
    ("Delete", "Clear selection / Delete layer"),
    ("Arrows", "Nudge"),
];

/// Fixed shortcuts that stay hard-wired in `keyboard.rs` (and one Windows
/// system key). A command may not be bound onto one of these; returns the name
/// of what the chord already does.
pub fn reserved_action(chord: KeyChord) -> Option<&'static str> {
    use KeyName as K;
    let KeyChord {
        ctrl,
        shift,
        alt,
        key,
    } = chord;
    let name = match (ctrl, shift, alt, key) {
        (false, false, false, K::X) => "Swap colours",
        (false, false, false, K::D) => "Reset colours",
        (true, false, false, K::D) => "Deselect / Repeat",
        (true, true, false, K::D) => "Repeat",
        (true, false, false, K::E) => "Merge Down",
        (true, true, false, K::E) => "Stamp Visible",
        (true, false, false, K::G) => "Group",
        (true, true, false, K::G) => "Ungroup",
        (true, false, true, K::G) => "Clipping Mask",
        (true, false, false, K::Q) => "Convert to Curves",
        (true, false, false, K::Y) => "Proof Colors",
        (true, true, false, K::Y) => "Gamut Warning",
        (true, true, false, K::X) => "Warp",
        (true, false, true, K::I) => "Image Size",
        (true, false, true, K::R) => "Refine Selection",
        (false, true, false, K::F3) => "Change Case",
        (false, true, false, K::F5) => "Smart Fill",
        (false, true, false, K::F6) => "Feather",
        (false, true, false, K::F7) => "Invert Selection",
        (false, false, true, K::F4) => "Close window (Windows)",
        _ => return None,
    };
    Some(name)
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

    /// The command saved under `id`, if this build knows it.
    pub fn from_id(id: &str) -> Option<Command> {
        Command::all().find(|cmd| cmd.id() == id)
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

/// Tag written into exported shortcuts files.
const SHORTCUTS_FILE_FORMAT: &str = "iai-shortcuts";

/// Why a chord cannot simply be given to a command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChordConflict {
    /// A fixed, non-rebindable shortcut already uses it (see [`reserved_action`]).
    Reserved(&'static str),
    /// Another re-bindable command holds it; it can be taken over.
    Command(Command),
}

/// The active bindings: every command in table order, with its chord or `None`
/// when the user removed its key. Chords are unique across the map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyMap {
    bindings: Vec<(Command, Option<KeyChord>)>,
}

impl Default for KeyMap {
    fn default() -> Self {
        Self {
            bindings: TABLE
                .iter()
                .map(|(cmd, chord, _, _)| (*cmd, Some(*chord)))
                .collect(),
        }
    }
}

impl KeyMap {
    /// Rebuild the map from the saved overrides (command id → chord label, `""`
    /// = no key). Self-healing: unknown ids are ignored, and an unreadable or
    /// reserved chord keeps that command's default. If two commands end up on
    /// one chord, a user-set binding beats a default one (the default loses its
    /// key); between two user-set ones the earlier row wins.
    pub fn from_overrides(overrides: &BTreeMap<String, String>) -> KeyMap {
        let mut map = KeyMap::default();
        let mut user_set = vec![false; map.bindings.len()];
        for (i, (cmd, slot)) in map.bindings.iter_mut().enumerate() {
            let Some(text) = overrides.get(cmd.id()) else {
                continue;
            };
            if text.trim().is_empty() {
                *slot = None;
                user_set[i] = true;
            } else if let Some(chord) =
                KeyChord::parse(text).filter(|c| reserved_action(*c).is_none())
            {
                *slot = Some(chord);
                user_set[i] = true;
            }
        }
        for i in 0..map.bindings.len() {
            for j in (i + 1)..map.bindings.len() {
                let Some(chord) = map.bindings[i].1 else {
                    break;
                };
                if map.bindings[j].1 == Some(chord) {
                    let loser = if user_set[j] && !user_set[i] { i } else { j };
                    map.bindings[loser].1 = None;
                }
            }
        }
        map
    }

    /// The overrides to persist: only rows that differ from their default.
    pub fn to_overrides(&self) -> BTreeMap<String, String> {
        self.bindings
            .iter()
            .filter(|(cmd, chord)| *chord != Some(cmd.default_chord()))
            .map(|(cmd, chord)| {
                (
                    cmd.id().to_string(),
                    chord.map(KeyChord::label).unwrap_or_default(),
                )
            })
            .collect()
    }

    /// The chord currently bound to `cmd`, if any.
    pub fn chord_for(&self, cmd: Command) -> Option<KeyChord> {
        self.bindings
            .iter()
            .find(|(c, _)| *c == cmd)
            .and_then(|(_, chord)| *chord)
    }

    /// The command a chord triggers, if any.
    pub fn command_for(&self, chord: KeyChord) -> Option<Command> {
        self.bindings
            .iter()
            .find(|(_, c)| *c == Some(chord))
            .map(|(cmd, _)| *cmd)
    }

    /// The command `chord` triggers only when that binding is the user's own —
    /// default bindings keep running through the original key arms.
    pub fn custom_command_for(&self, chord: KeyChord) -> Option<Command> {
        self.command_for(chord).filter(|cmd| !self.is_default(*cmd))
    }

    /// Label for `cmd`'s current binding (empty when it has no key).
    pub fn label_for(&self, cmd: Command) -> String {
        self.chord_for(cmd).map(KeyChord::label).unwrap_or_default()
    }

    /// `cmd` still has its built-in chord.
    pub fn is_default(&self, cmd: Command) -> bool {
        self.chord_for(cmd) == Some(cmd.default_chord())
    }

    /// Why `chord` cannot go to `cmd` as-is, if anything stands in the way.
    pub fn conflict(&self, cmd: Command, chord: KeyChord) -> Option<ChordConflict> {
        if let Some(name) = reserved_action(chord) {
            return Some(ChordConflict::Reserved(name));
        }
        self.command_for(chord)
            .filter(|other| *other != cmd)
            .map(ChordConflict::Command)
    }

    /// The shortcuts file written by Preferences ▸ Shortcuts ▸ Export: only
    /// the changed bindings, like `prefs.json`, tagged so it can be recognised.
    pub fn export_json(&self) -> String {
        let value = serde_json::json!({
            "format": SHORTCUTS_FILE_FORMAT,
            "version": 1,
            "shortcuts": self.to_overrides(),
        });
        serde_json::to_string_pretty(&value).unwrap_or_default()
    }

    /// Read a shortcuts file. The result replaces the whole keymap (commands the
    /// file does not mention keep their built-in key) and is repaired like
    /// `prefs.json`; also returns how many entries could not be used. Errors
    /// only when the text is not a shortcuts file at all.
    pub fn import_json(text: &str) -> Result<(KeyMap, usize), String> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|_| "File không phải JSON hợp lệ.".to_string())?;
        let entries = match value.get("shortcuts") {
            Some(serde_json::Value::Object(map)) => map,
            _ => return Err("File không phải bộ phím tắt của iAi.".to_string()),
        };
        let overrides: BTreeMap<String, String> = entries
            .iter()
            .filter_map(|(id, v)| v.as_str().map(|s| (id.clone(), s.to_string())))
            .collect();
        let keymap = KeyMap::from_overrides(&overrides);
        let used = overrides
            .iter()
            .filter(|(id, text)| {
                Command::from_id(id).is_some_and(|cmd| {
                    let wanted = if text.trim().is_empty() {
                        Some(String::new())
                    } else {
                        KeyChord::parse(text).map(KeyChord::label)
                    };
                    wanted == Some(keymap.label_for(cmd))
                })
            })
            .count();
        Ok((keymap, entries.len() - used))
    }

    /// Give `chord` (or no key) to `cmd`. Any other command holding that chord
    /// loses it, so chords stay unique. Callers check [`Self::conflict`] first
    /// and never pass a reserved chord.
    pub fn assign(&mut self, cmd: Command, chord: Option<KeyChord>) {
        for (other, slot) in &mut self.bindings {
            if *other != cmd && chord.is_some() && *slot == chord {
                *slot = None;
            }
        }
        if let Some((_, slot)) = self.bindings.iter_mut().find(|(c, _)| *c == cmd) {
            *slot = chord;
        }
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

    fn overrides(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn chord_labels_parse_back_for_every_key_and_modifier() {
        for key in KeyName::ALL {
            for bits in 0..8u8 {
                let chord = KeyChord::new(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, key);
                assert_eq!(
                    KeyChord::parse(&chord.label()),
                    Some(chord),
                    "{}",
                    chord.label()
                );
            }
        }
        assert_eq!(
            KeyChord::parse("ctrl + shift + k"),
            Some(KeyChord::ctrl_shift(KeyName::K))
        );
        for bad in ["", "Ctrl+", "Ctrl+Ctrl+K", "Hyper+K", "Ctrl+Enter", "KK"] {
            assert_eq!(KeyChord::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn fixed_key_list_matches_the_reserved_chords() {
        for (keys, action) in FIXED_SHORTCUTS {
            if let Some(chord) = KeyChord::parse(keys) {
                assert_eq!(reserved_action(chord), Some(*action), "{keys}");
            }
        }
    }

    #[test]
    fn no_default_binding_is_a_reserved_chord() {
        for cmd in Command::all() {
            assert_eq!(reserved_action(cmd.default_chord()), None, "{cmd:?}");
        }
    }

    #[test]
    fn untouched_keymap_saves_no_overrides_and_has_no_custom_commands() {
        let km = KeyMap::from_overrides(&BTreeMap::new());
        assert_eq!(km, KeyMap::default());
        assert!(km.to_overrides().is_empty());
        for cmd in Command::all() {
            assert!(km.is_default(cmd));
            assert_eq!(km.custom_command_for(cmd.default_chord()), None);
        }
    }

    #[test]
    fn overrides_round_trip_including_a_removed_key() {
        let mut km = KeyMap::default();
        km.assign(Command::ToolBrush, Some(KeyChord::plain(KeyName::Q)));
        km.assign(Command::FileSave, None);
        let saved = km.to_overrides();
        assert_eq!(saved, overrides(&[("file.save", ""), ("tool.brush", "Q")]));
        let loaded = KeyMap::from_overrides(&saved);
        assert_eq!(loaded, km);
        assert_eq!(
            loaded.custom_command_for(KeyChord::plain(KeyName::Q)),
            Some(Command::ToolBrush)
        );
        assert_eq!(loaded.chord_for(Command::FileSave), None);
        assert_eq!(loaded.command_for(KeyChord::plain(KeyName::B)), None);
    }

    #[test]
    fn a_damaged_file_falls_back_to_defaults() {
        let km = KeyMap::from_overrides(&overrides(&[
            ("tool.nonexistent", "Q"),
            ("tool.brush", "Ctrl+Hyper+?"),
            ("file.save", "Ctrl+G"), // reserved: Group
        ]));
        assert_eq!(km, KeyMap::default());
    }

    #[test]
    fn a_user_binding_takes_the_key_from_a_default_one() {
        // Brush was moved onto E without the file recording Eraser losing it.
        let km = KeyMap::from_overrides(&overrides(&[("tool.brush", "E")]));
        assert_eq!(
            km.chord_for(Command::ToolBrush),
            Some(KeyChord::plain(KeyName::E))
        );
        assert_eq!(km.chord_for(Command::ToolEraser), None);
    }

    #[test]
    fn two_user_bindings_on_one_key_keep_the_first_row() {
        let km = KeyMap::from_overrides(&overrides(&[("tool.brush", "Q"), ("tool.eraser", "Q")]));
        assert_eq!(
            km.chord_for(Command::ToolBrush),
            Some(KeyChord::plain(KeyName::Q))
        );
        assert_eq!(km.chord_for(Command::ToolEraser), None);
    }

    #[test]
    fn swapped_keys_survive_a_reload() {
        let mut km = KeyMap::default();
        km.assign(Command::ToolBrush, Some(KeyChord::plain(KeyName::E)));
        km.assign(Command::ToolEraser, Some(KeyChord::plain(KeyName::B)));
        assert_eq!(KeyMap::from_overrides(&km.to_overrides()), km);
    }

    #[test]
    fn conflicts_name_the_reserved_action_or_the_other_command() {
        let km = KeyMap::default();
        assert_eq!(
            km.conflict(Command::ToolBrush, KeyChord::plain(KeyName::X)),
            Some(ChordConflict::Reserved("Swap colours"))
        );
        assert_eq!(
            km.conflict(Command::ToolBrush, KeyChord::plain(KeyName::E)),
            Some(ChordConflict::Command(Command::ToolEraser))
        );
        assert_eq!(
            km.conflict(Command::ToolBrush, KeyChord::plain(KeyName::B)),
            None
        );
        assert_eq!(
            km.conflict(Command::ToolBrush, KeyChord::plain(KeyName::Q)),
            None
        );
    }

    #[test]
    fn assigning_a_taken_key_moves_it() {
        let mut km = KeyMap::default();
        km.assign(Command::ToolBrush, Some(KeyChord::plain(KeyName::E)));
        assert_eq!(km.chord_for(Command::ToolEraser), None);
        assert_eq!(
            km.command_for(KeyChord::plain(KeyName::E)),
            Some(Command::ToolBrush)
        );
        // Resetting Eraser to its default then takes E back from Brush.
        km.assign(
            Command::ToolEraser,
            Some(Command::ToolEraser.default_chord()),
        );
        assert_eq!(km.chord_for(Command::ToolBrush), None);
        assert!(km.is_default(Command::ToolEraser));
    }

    #[test]
    fn exported_shortcuts_import_back_exactly() {
        let mut km = KeyMap::default();
        km.assign(Command::ToolBrush, Some(KeyChord::plain(KeyName::Q)));
        km.assign(Command::FileSave, Some(KeyName::F2).map(KeyChord::plain));
        km.assign(Command::Invert, None);
        let (back, ignored) = KeyMap::import_json(&km.export_json()).unwrap();
        assert_eq!(back, km);
        assert_eq!(ignored, 0);
        // An untouched keymap exports as "all defaults" and imports as such.
        let (fresh, _) = KeyMap::import_json(&KeyMap::default().export_json()).unwrap();
        assert_eq!(fresh, KeyMap::default());
    }

    #[test]
    fn importing_reports_unusable_entries_and_rejects_foreign_files() {
        let text = r#"{ "format": "iai-shortcuts", "version": 1, "shortcuts": {
            "tool.brush": "Q",
            "tool.eraser": "E",
            "tool.teleport": "T",
            "file.save": "Ctrl+G",
            "file.open": 7
        } }"#;
        let (km, ignored) = KeyMap::import_json(text).unwrap();
        assert_eq!(
            km.chord_for(Command::ToolBrush),
            Some(KeyChord::plain(KeyName::Q))
        );
        assert!(km.is_default(Command::ToolEraser));
        assert!(
            km.is_default(Command::FileSave),
            "a fixed key keeps the default"
        );
        assert_eq!(ignored, 3, "unknown id, fixed key and non-text value");

        assert!(KeyMap::import_json("not json").is_err());
        assert!(KeyMap::import_json(r#"{ "theme_mode": "Dark" }"#).is_err());
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
