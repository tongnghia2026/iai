use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizePreset {
    pub name: String,
    pub width: f32,
    pub height: f32,
    pub unit: String,
    pub dpi: f32,
}

impl SizePreset {
    pub fn unit_enum(&self) -> crate::core::units::Unit {
        match self.unit.as_str() {
            "cm" => crate::core::units::Unit::Centimeters,
            "mm" => crate::core::units::Unit::Millimeters,
            "in" => crate::core::units::Unit::Inches,
            "pt" => crate::core::units::Unit::Points,
            "pc" => crate::core::units::Unit::Picas,
            "%" => crate::core::units::Unit::Percent,
            _ => crate::core::units::Unit::Pixels,
        }
    }

    pub fn unit_idx(&self) -> u8 {
        match self.unit.as_str() {
            "px" => 0,
            "cm" => 1,
            "mm" => 2,
            "in" => 3,
            "pt" => 4,
            "pc" => 5,
            "%" => 6,
            _ => 0,
        }
    }

    pub fn pixel_width_for(&self, canvas_w: f32) -> f32 {
        crate::core::units::to_pixels(self.width, self.unit_enum(), self.dpi, canvas_w).max(1.0)
    }

    pub fn pixel_height_for(&self, canvas_h: f32) -> f32 {
        crate::core::units::to_pixels(self.height, self.unit_enum(), self.dpi, canvas_h).max(1.0)
    }

    fn presets_path() -> Option<std::path::PathBuf> {
        let dir = std::env::var("APPDATA")
            .map(|p| std::path::PathBuf::from(p).join("IAI"))
            .unwrap_or_else(|_| std::path::PathBuf::from(".iai"));
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join("presets.json"))
    }

    pub fn save_all(presets: &[SizePreset]) {
        if let Some(path) = Self::presets_path() {
            if let Ok(json) = serde_json::to_string_pretty(presets) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    pub fn load_all() -> Vec<SizePreset> {
        if let Some(path) = Self::presets_path() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(presets) = serde_json::from_str::<Vec<SizePreset>>(&content) {
                    if !presets.is_empty() {
                        return presets;
                    }
                }
            }
        }
        Self::defaults()
    }

    pub fn defaults() -> Vec<SizePreset> {
        vec![
            SizePreset {
                name: "A4 Portrait 300DPI".into(),
                width: 2480.0,
                height: 3508.0,
                unit: "px".into(),
                dpi: 300.0,
            },
            SizePreset {
                name: "A4 Landscape 300DPI".into(),
                width: 3508.0,
                height: 2480.0,
                unit: "px".into(),
                dpi: 300.0,
            },
            SizePreset {
                name: "1920x1080 72DPI".into(),
                width: 1920.0,
                height: 1080.0,
                unit: "px".into(),
                dpi: 72.0,
            },
            SizePreset {
                name: "Instagram Square".into(),
                width: 1080.0,
                height: 1080.0,
                unit: "px".into(),
                dpi: 72.0,
            },
        ]
    }
}

/// A named, user-saved set of Develop sliders. `DevelopSettings` serializes
/// with `serde(default)`, so saved presets stay loadable as sliders are added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevelopPreset {
    pub name: String,
    pub settings: crate::core::develop::DevelopSettings,
}

impl DevelopPreset {
    fn presets_path() -> Option<std::path::PathBuf> {
        let dir = std::env::var("APPDATA")
            .map(|p| std::path::PathBuf::from(p).join("IAI"))
            .unwrap_or_else(|_| std::path::PathBuf::from(".iai"));
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join("develop_presets.json"))
    }

    pub fn save_all(presets: &[DevelopPreset]) {
        if let Some(path) = Self::presets_path() {
            if let Ok(json) = serde_json::to_string_pretty(presets) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    pub fn load_all() -> Vec<DevelopPreset> {
        if let Some(path) = Self::presets_path() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(presets) = serde_json::from_str::<Vec<DevelopPreset>>(&content) {
                    return presets;
                }
            }
        }
        Vec::new()
    }
}

/// A named, user-saved Levels configuration ([master, R, G, B] channels).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LevelsPreset {
    pub name: String,
    pub channels: [crate::core::layer::LevelsParams; 4],
}

/// A named, user-saved Curves configuration ([master, R, G, B] channels).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurvesPreset {
    pub name: String,
    pub channels: [Vec<(f32, f32)>; 4],
}

/// User-saved Levels/Curves presets, persisted together in
/// %APPDATA%/IAI/adjustment_presets.json. Built-in presets are compiled into
/// the dialogs and never stored here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdjustmentPresets {
    #[serde(default)]
    pub levels: Vec<LevelsPreset>,
    #[serde(default)]
    pub curves: Vec<CurvesPreset>,
}

impl AdjustmentPresets {
    fn presets_path() -> Option<std::path::PathBuf> {
        let dir = std::env::var("APPDATA")
            .map(|p| std::path::PathBuf::from(p).join("IAI"))
            .unwrap_or_else(|_| std::path::PathBuf::from(".iai"));
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join("adjustment_presets.json"))
    }

    pub fn save(&self) {
        if let Some(path) = Self::presets_path() {
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    pub fn load() -> Self {
        if let Some(path) = Self::presets_path() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(presets) = serde_json::from_str::<AdjustmentPresets>(&content) {
                    return presets;
                }
            }
        }
        Self::default()
    }

    /// Insert or overwrite the Levels preset with this name.
    pub fn upsert_levels(&mut self, name: String, channels: [crate::core::layer::LevelsParams; 4]) {
        if let Some(existing) = self.levels.iter_mut().find(|p| p.name == name) {
            existing.channels = channels;
        } else {
            self.levels.push(LevelsPreset { name, channels });
        }
    }

    /// Insert or overwrite the Curves preset with this name.
    pub fn upsert_curves(&mut self, name: String, channels: [Vec<(f32, f32)>; 4]) {
        if let Some(existing) = self.curves.iter_mut().find(|p| p.name == name) {
            existing.channels = channels;
        } else {
            self.curves.push(CurvesPreset { name, channels });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn develop_preset_roundtrips_through_json() {
        let mut settings = crate::core::develop::DevelopSettings::default();
        settings.exposure = 25.0;
        settings.temperature = -10.0;
        settings.curve_points = vec![[0.0, 0.0], [0.4, 0.55], [1.0, 1.0]];
        let preset = DevelopPreset {
            name: "Punchy".into(),
            settings,
        };
        let json = serde_json::to_string(&vec![preset.clone()]).unwrap();
        let back: Vec<DevelopPreset> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "Punchy");
        assert_eq!(back[0].settings, preset.settings);
    }

    #[test]
    fn percent_preset_uses_reference_canvas_size() {
        let preset = SizePreset {
            name: "Half".to_string(),
            width: 50.0,
            height: 25.0,
            unit: "%".to_string(),
            dpi: 72.0,
        };

        assert_eq!(preset.pixel_width_for(800.0), 400.0);
        assert_eq!(preset.pixel_height_for(600.0), 150.0);
    }

    #[test]
    fn adjustment_presets_roundtrip_through_json() {
        let mut levels = [crate::core::layer::LevelsParams::default(); 4];
        levels[1].in_black = 12;
        levels[1].gamma = 1.4;
        let mut curves: [Vec<(f32, f32)>; 4] =
            std::array::from_fn(|_| crate::core::layer::identity_curve());
        curves[0] = vec![(0.0, 0.0), (0.4, 0.6), (1.0, 1.0)];

        let mut presets = AdjustmentPresets::default();
        presets.upsert_levels("Warm shadows".into(), levels);
        presets.upsert_curves("Punch".into(), curves.clone());

        let json = serde_json::to_string(&presets).unwrap();
        let back: AdjustmentPresets = serde_json::from_str(&json).unwrap();
        assert_eq!(back, presets);

        // Legacy/partial files: missing sections default to empty.
        let partial: AdjustmentPresets = serde_json::from_str("{}").unwrap();
        assert!(partial.levels.is_empty() && partial.curves.is_empty());
    }

    #[test]
    fn adjustment_preset_upsert_overwrites_same_name() {
        let mut presets = AdjustmentPresets::default();
        let mut a = [crate::core::layer::LevelsParams::default(); 4];
        a[0].gamma = 0.8;
        let mut b = [crate::core::layer::LevelsParams::default(); 4];
        b[0].gamma = 1.3;
        presets.upsert_levels("Mine".into(), a);
        presets.upsert_levels("Mine".into(), b);
        assert_eq!(presets.levels.len(), 1);
        assert_eq!(presets.levels[0].channels[0].gamma, 1.3);
    }
}
