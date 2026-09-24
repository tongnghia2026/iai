//! Persisted application settings (Preferences).
//!
//! Stored in the shared `%APPDATA%/IAI/prefs.json` (same file as the theme and a
//! few dialog prefs), written key-by-key with a merge so unrelated keys survive.
//! Every field carries `#[serde(default)]` and a fallback, so a partial or
//! corrupt file silently falls back to the built-in default instead of losing
//! the user's settings. Values are clamped to safe ranges on apply.

use serde::{Deserialize, Serialize};

use crate::core::units::Unit;

/// Autosave period limits, in seconds.
pub const AUTOSAVE_MIN_SECS: u32 = 30;
pub const AUTOSAVE_MAX_SECS: u32 = 600;

fn default_true() -> bool {
    true
}
fn default_autosave_secs() -> u32 {
    90
}
fn default_unit() -> Unit {
    Unit::Pixels
}

/// Everything the Preferences dialog reads and writes. Extended over time; each
/// field is independent and optional in the file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    /// Preferred measurement unit for rulers and new/size dialogs.
    #[serde(default = "default_unit")]
    pub default_unit: Unit,
    /// Mirror the active document to disk on a timer for crash recovery.
    #[serde(default = "default_true")]
    pub autosave_enabled: bool,
    /// Autosave period in seconds.
    #[serde(default = "default_autosave_secs")]
    pub autosave_interval_secs: u32,
    /// Startup value of the snapping toggle (guides / layer move / transform).
    #[serde(default)]
    pub snap_default: bool,
    /// Let the AI features (Select Subject, Smart Fill) run on the GPU via
    /// DirectML when a capable adapter is present.
    #[serde(default = "default_true")]
    pub ai_use_gpu: bool,
    /// Customised shortcuts only: command id → chord label (`""` = no key).
    /// Commands not listed keep their built-in key. Interpreted and repaired by
    /// `app::commands::KeyMap::from_overrides`.
    #[serde(default)]
    pub shortcuts: std::collections::BTreeMap<String, String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_unit: default_unit(),
            autosave_enabled: default_true(),
            autosave_interval_secs: default_autosave_secs(),
            snap_default: false,
            ai_use_gpu: default_true(),
            shortcuts: Default::default(),
        }
    }
}

impl AppSettings {
    /// Clamp every field to its safe range. Called after load and before apply so
    /// a hand-edited or stale file can never push the app into a bad state.
    pub fn sanitize(&mut self) {
        self.autosave_interval_secs = self
            .autosave_interval_secs
            .clamp(AUTOSAVE_MIN_SECS, AUTOSAVE_MAX_SECS);
    }

    /// Load from `prefs.json`, falling back to defaults on any read/parse error.
    pub fn load() -> Self {
        let text = std::fs::read_to_string(crate::ui::theme::prefs_path()).unwrap_or_default();
        Self::load_from_str(&text)
    }

    /// Parse `prefs.json` text. A malformed `shortcuts` entry (wrong shape or
    /// non-text values) is dropped on its own — the shortcuts fall back to their
    /// defaults — instead of discarding every other preference with it.
    fn load_from_str(text: &str) -> Self {
        let mut settings = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .map(|mut value| {
                if let Some(obj) = value.as_object_mut() {
                    let shortcuts: serde_json::Map<String, serde_json::Value> =
                        match obj.remove("shortcuts") {
                            Some(serde_json::Value::Object(map)) => {
                                map.into_iter().filter(|(_, v)| v.is_string()).collect()
                            }
                            _ => serde_json::Map::new(),
                        };
                    obj.insert("shortcuts".into(), serde_json::Value::Object(shortcuts));
                }
                value
            })
            .and_then(|value| serde_json::from_value::<AppSettings>(value).ok())
            .unwrap_or_default();
        settings.sanitize();
        settings
    }

    /// Persist to `prefs.json`, merging into the existing object so keys owned by
    /// other subsystems (theme, adjustment prefs, last CMYK profile) are kept.
    /// Best-effort — a write failure is non-fatal.
    pub fn save(&self) {
        let path = crate::ui::theme::prefs_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
        if !value.is_object() {
            value = serde_json::Value::Object(Default::default());
        }
        if let (Some(map), Ok(serde_json::Value::Object(mine))) =
            (value.as_object_mut(), serde_json::to_value(self))
        {
            for (key, v) in mine {
                map.insert(key, v);
            }
        }
        if let Ok(json) = serde_json::to_string_pretty(&value) {
            let _ = std::fs::write(&path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_in_range() {
        let mut s = AppSettings::default();
        let before = s.clone();
        s.sanitize();
        assert_eq!(s, before, "defaults must already be within their clamps");
        assert_eq!(s.autosave_interval_secs, 90);
        assert!(s.autosave_enabled);
        assert!(s.ai_use_gpu);
    }

    #[test]
    fn sanitize_clamps_out_of_range_values() {
        let mut s = AppSettings {
            autosave_interval_secs: 5,
            ..AppSettings::default()
        };
        s.sanitize();
        assert_eq!(s.autosave_interval_secs, AUTOSAVE_MIN_SECS);

        let mut low = AppSettings {
            autosave_interval_secs: 100_000,
            ..AppSettings::default()
        };
        low.sanitize();
        assert_eq!(low.autosave_interval_secs, AUTOSAVE_MAX_SECS);
    }

    #[test]
    fn partial_json_fills_missing_fields_with_defaults() {
        let json = r#"{ "default_unit": "Centimeters" }"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.default_unit, Unit::Centimeters);
        assert_eq!(s.autosave_interval_secs, 90);
        assert!(s.autosave_enabled);
        assert!(s.shortcuts.is_empty());
    }

    #[test]
    fn shortcut_overrides_round_trip_through_json() {
        let mut s = AppSettings::default();
        s.shortcuts.insert("tool.brush".into(), "Q".into());
        s.shortcuts.insert("file.save".into(), String::new());
        let json = serde_json::to_string(&s).unwrap();
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn a_malformed_shortcuts_entry_keeps_the_rest_of_the_settings() {
        // `shortcuts` must be an object of strings; a wrong shape must not wipe
        // every other preference.
        let json = r#"{ "default_unit": "Centimeters", "shortcuts": 42 }"#;
        let s = AppSettings::load_from_str(json);
        assert_eq!(s.default_unit, Unit::Centimeters);
        assert!(s.shortcuts.is_empty());

        let json = r#"{ "autosave_interval_secs": 120,
                        "shortcuts": { "tool.brush": "Q", "tool.eraser": 5 } }"#;
        let s = AppSettings::load_from_str(json);
        assert_eq!(s.autosave_interval_secs, 120);
        assert_eq!(s.shortcuts.len(), 1);
        assert_eq!(s.shortcuts["tool.brush"], "Q");
    }

    #[test]
    fn unreadable_file_gives_defaults() {
        assert_eq!(AppSettings::load_from_str(""), AppSettings::default());
        assert_eq!(
            AppSettings::load_from_str("{ not json"),
            AppSettings::default()
        );
    }
}
