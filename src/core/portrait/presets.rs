//! "Công thức": named sets of the Chỉnh chân dung sliders. A fresh install is
//! given a few to start from; the owner changes, adds and deletes them.

use serde::{Deserialize, Serialize};

use super::effects::PortraitSettings;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub settings: PortraitSettings,
}

/// Bumped when `built_in` gains presets, so an install that already took an
/// earlier round is given the new ones once (and deleted ones stay deleted).
pub const BUILT_IN_ROUND: u32 = 1;

/// The presets an install starts with. None changes the face's shape: they
/// are for ID photos.
pub fn built_in() -> Vec<Preset> {
    let base = PortraitSettings::default();
    let preset = |name: &str, settings: PortraitSettings| Preset {
        name: name.to_string(),
        settings,
    };
    vec![
        preset(
            "Ảnh thẻ nữ",
            PortraitSettings {
                smooth: 55.0,
                volume: 35.0,
                even_tone: 35.0,
                shine: 30.0,
                brighten: 10.0,
                blemish: 75.0,
                dark_circles: 45.0,
                eye_white: 30.0,
                iris: 20.0,
                teeth: 30.0,
                lip_saturation: 15.0,
                ..base
            },
        ),
        preset(
            "Ảnh thẻ nam",
            PortraitSettings {
                smooth: 25.0,
                volume: 40.0,
                even_tone: 20.0,
                shine: 30.0,
                dark_circles: 25.0,
                eye_white: 20.0,
                iris: 10.0,
                teeth: 20.0,
                sharpen: 30.0,
                look_strength: 60.0,
                ..base
            },
        ),
        preset(
            "Trẻ em",
            PortraitSettings {
                smooth: 10.0,
                volume: 10.0,
                ai_detail: 40.0,
                even_tone: 10.0,
                shine: 10.0,
                blemish: 30.0,
                dark_circles: 0.0,
                eye_white: 15.0,
                teeth: 0.0,
                sharpen: 15.0,
                look_strength: 60.0,
                ..base
            },
        ),
        preset(
            "Lớn tuổi",
            PortraitSettings {
                smooth: 50.0,
                volume: 45.0,
                even_tone: 35.0,
                brighten: 8.0,
                blemish: 70.0,
                dark_circles: 45.0,
                teeth: 30.0,
                ..base
            },
        ),
        preset(
            "Nhẹ, tự nhiên",
            PortraitSettings {
                smooth: 20.0,
                volume: 20.0,
                ai_detail: 40.0,
                even_tone: 15.0,
                shine: 15.0,
                blemish: 40.0,
                dark_circles: 15.0,
                eye_white: 10.0,
                iris: 10.0,
                teeth: 10.0,
                sharpen: 15.0,
                look_strength: 50.0,
                ..base
            },
        ),
    ]
}

/// Add the built-in presets whose names are not taken.
pub fn add_built_in(presets: &mut Vec<Preset>) {
    for preset in built_in() {
        if !presets.iter().any(|p| p.name == preset.name) {
            presets.push(preset);
        }
    }
}

/// Keep `settings` under `name`, in place of the preset of that name.
pub fn keep(presets: &mut Vec<Preset>, name: &str, settings: PortraitSettings) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    match presets.iter_mut().find(|p| p.name == name) {
        Some(preset) => preset.settings = settings,
        None => presets.push(Preset {
            name: name.to_string(),
            settings,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_is_kept_by_name_and_read_back() {
        let mut presets = Vec::new();
        let soft = PortraitSettings {
            smooth: 30.0,
            ..PortraitSettings::NEUTRAL
        };
        keep(&mut presets, "  Nữ ", soft);
        keep(&mut presets, "Nam", PortraitSettings::default());
        keep(&mut presets, "  ", soft);
        let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Nữ", "Nam"]);
        // The same name again replaces what it kept.
        let softer = PortraitSettings {
            smooth: 60.0,
            ..soft
        };
        keep(&mut presets, "Nữ", softer);
        assert_eq!((presets.len(), presets[0].settings), (2, softer));
        let json = serde_json::to_string(&presets).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Preset>>(&json).unwrap(), presets);
        // A preset kept before a slider existed reads that slider's default.
        let old: Vec<Preset> =
            serde_json::from_str(r#"[{"name":"Cũ","settings":{"smooth":40.0}}]"#).unwrap();
        assert_eq!(old[0].settings.smooth, 40.0);
    }

    #[test]
    fn the_built_in_presets_are_distinct_and_leave_the_face_shape_alone() {
        let presets = built_in();
        assert!(presets.len() >= 3);
        let shape = |s: &PortraitSettings| {
            [
                s.face_slim,
                s.face_squeeze,
                s.chin_length,
                s.forehead_height,
                s.eye_size,
                s.eye_tilt,
                s.nose_slim,
                s.nose_bridge,
                s.mouth_width,
                s.smile,
                s.lip_fullness,
                s.body_waist,
                s.body_shoulders,
                s.body_neck,
                s.body_arms,
                s.body_legs,
                s.body_leg_length,
            ]
        };
        for (i, a) in presets.iter().enumerate() {
            assert!(shape(&a.settings).iter().all(|&v| v == 0.0), "{}", a.name);
            // Each shows its own name in the dialog: none is the defaults or
            // another preset.
            assert_ne!(a.settings, PortraitSettings::default(), "{}", a.name);
            for b in &presets[i + 1..] {
                assert_ne!(a.name, b.name);
                assert_ne!(a.settings, b.settings, "{} / {}", a.name, b.name);
            }
        }
    }

    /// Opt-in visual probe: IAI_PORTRAIT_PRESET_PROBE is a folder of photos;
    /// each gets `preset_<name>.png`: as shot, then every built-in preset
    /// (without the AI detail), left to right.
    #[test]
    #[ignore]
    fn probe_built_in_presets() {
        use crate::core::portrait::{analyze, correct, looks, render};
        let Ok(dir) = std::env::var("IAI_PORTRAIT_PRESET_PROBE") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.starts_with("preset_") || !(name.ends_with(".jpg") || name.ends_with(".png")) {
                continue;
            }
            let image = image::open(&path).unwrap().to_rgba8();
            let (w, h) = image.dimensions();
            let rgba = image.into_raw();
            let model = analyze(&rgba, w, h, false, None, &|_| {}).unwrap();
            let enabled = vec![true; model.faces.len()];
            let mut views = vec![rgba.clone()];
            for preset in built_in() {
                let settings = PortraitSettings {
                    ai_detail: 0.0,
                    ..preset.settings
                };
                let retouched = render(&rgba, &model, &settings, &enabled, &[]);
                let mut full = looks::with_retouch(&rgba, w, retouched);
                let fix = settings
                    .fixes()
                    .and_then(|fixes| correct::fix_lut(&model.light, &fixes));
                let look = settings
                    .studio_look()
                    .and_then(|(look, strength)| Some((looks::LookLut::new(look)?, strength)));
                let look = look.as_ref().map(|(lut, strength)| (lut, *strength));
                looks::grade(&mut full, w, fix.as_ref(), look, None);
                views.push(full);
            }
            let mut sheet = image::RgbaImage::new((w + 8) * views.len() as u32, h);
            for (k, view) in views.into_iter().enumerate() {
                let view = image::RgbaImage::from_raw(w, h, view).unwrap();
                image::imageops::replace(&mut sheet, &view, (k as u32 * (w + 8)) as i64, 0);
            }
            sheet.save(dir.join(format!("preset_{name}.png"))).unwrap();
        }
    }

    #[test]
    fn built_ins_are_added_beside_the_owners_presets_not_over_them() {
        let mine = PortraitSettings {
            smooth: 77.0,
            ..PortraitSettings::default()
        };
        let mut presets = vec![Preset {
            name: "Ảnh thẻ nữ".to_string(),
            settings: mine,
        }];
        add_built_in(&mut presets);
        assert_eq!(presets.len(), built_in().len());
        assert_eq!(presets[0].settings, mine);
        // Twice changes nothing.
        let once = presets.clone();
        add_built_in(&mut presets);
        assert_eq!(presets, once);
    }
}
