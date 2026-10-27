//! Image ▸ Mode and Image ▸ Color Profile: bits per channel, RGB or CMYK,
//! and the colour profile the pixels are read in.

use std::path::Path;

use iai_canvas::canvas::{BitDepth, CmykProfile, ColorMode};
use iai_color::cms::WorkingProfile;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::common::not_enabled;
use crate::{
    CmdError, Command, CommandMeta, Effect, EngineEvent, ErrorCode, Plan, Session, Validate,
};

/// A document's mode after a change.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Moded {
    pub doc: u32,
    /// 8 or 16.
    pub bits: u8,
    /// "rgb" or "cmyk".
    pub color: String,
}

fn moded(s: &mut Session<'_>, doc: u32) -> Result<Moded, CmdError> {
    let canvas = s.canvas(Some(doc))?;
    let out = Moded {
        doc,
        bits: if canvas.bit_depth == BitDepth::Sixteen {
            16
        } else {
            8
        },
        color: if canvas.is_cmyk() { "cmyk" } else { "rgb" }.to_string(),
    };
    s.emit(EngineEvent::Structure { doc });
    Ok(out)
}

/// The colour mode `image.mode` turns a document to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColorTo {
    Rgb,
    Cmyk,
}

/// A CMYK profile from a file.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IccFile {
    /// An `.icc` / `.icm` CMYK profile.
    #[schemars(length(min = 1, max = 4096))]
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModeParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    /// Bits per channel: 8 or 16.
    pub bits: Option<u8>,
    /// RGB, or CMYK for print (the built-in Generic space, or `icc`).
    pub color: Option<ColorTo>,
    /// For `color: cmyk`: the printer's CMYK profile.
    pub icc: Option<IccFile>,
}

impl Validate for ModeParams {
    fn validate(&self) -> Result<(), CmdError> {
        if self.bits.is_none() && self.color.is_none() {
            return Err(CmdError::bad_param("(params)", "bits or color"));
        }
        if self.bits.is_some_and(|b| b != 8 && b != 16) {
            return Err(CmdError::bad_param("bits", "8 or 16"));
        }
        if self.icc.is_some() && self.color != Some(ColorTo::Cmyk) {
            return Err(CmdError::bad_param("icc", "only with color = cmyk"));
        }
        Ok(())
    }
}

static MODE: CommandMeta = CommandMeta {
    id: "image.mode",
    label_vi: "Chế độ màu",
    label_en: "Mode",
    doc: "Image ▸ Mode: 8 or 16 bits per channel, and RGB or CMYK (the built-in Generic CMYK, \
          or a printer's profile `icc`). Undoable.",
    effects: &[Effect::Edit, Effect::ReadFiles],
    featured: false,
};

pub struct Mode;

impl Command for Mode {
    type Params = ModeParams;
    type Output = Moded;
    fn meta(&self) -> &'static CommandMeta {
        &MODE
    }
    fn plan(&self, s: &mut Session<'_>, p: ModeParams) -> Result<Plan<Moded>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        // The profile is read before anything changes.
        let profile = match &p.icc {
            Some(file) => {
                let path = Path::new(&file.path);
                let data = std::fs::read(path).map_err(|e| {
                    CmdError::new(ErrorCode::NotFound, format!("{}: {e}", path.display()))
                        .vi(format!("Could not read ICC file: {e}"))
                })?;
                if !iai_color::cms::profile_is_cmyk(&data) {
                    return Err(not_enabled(
                        "the file is not a CMYK profile",
                        "The selected ICC file is not a CMYK profile",
                    ));
                }
                let name = path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("CMYK profile")
                    .to_string();
                CmykProfile::Icc { name, data }
            }
            None => CmykProfile::Naive,
        };
        let canvas = s.canvas_mut(Some(doc))?;
        let mut said = None;
        if let Some(bits) = p.bits {
            canvas.set_bit_depth(bits == 16);
            said = Some(if bits == 16 {
                "Mode: 16 Bits/Channel".to_string()
            } else {
                "Mode: 8 Bits/Channel".to_string()
            });
        }
        match p.color {
            Some(ColorTo::Rgb) => {
                canvas.convert_to_rgb_mode();
                said = Some("Mode: RGB".to_string());
            }
            Some(ColorTo::Cmyk) => {
                canvas
                    .convert_to_cmyk(profile)
                    .map_err(|e| not_enabled(e.clone(), format!("CMYK conversion failed: {e}")))?;
                let name = match &canvas.color_mode {
                    ColorMode::Cmyk(p) => p.display_name().to_string(),
                    ColorMode::Rgb => String::new(),
                };
                said = Some(format!("Mode: CMYK — {name}"));
            }
            None => {}
        }
        if let Some(line) = said {
            s.notice(line.clone(), line);
        }
        s.emit(EngineEvent::Pixels { doc });
        moded(s, doc).map(Plan::Done)
    }
}

/// A working colour profile, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Srgb,
    AdobeRgb,
    DisplayP3,
    ProPhoto,
}

impl Profile {
    fn core(self) -> WorkingProfile {
        match self {
            Profile::Srgb => WorkingProfile::Srgb,
            Profile::AdobeRgb => WorkingProfile::AdobeRgb,
            Profile::DisplayP3 => WorkingProfile::DisplayP3,
            Profile::ProPhoto => WorkingProfile::ProPhoto,
        }
    }

    /// The name `image.profile` takes for `profile`.
    pub fn of(profile: WorkingProfile) -> Self {
        match profile {
            WorkingProfile::Srgb => Profile::Srgb,
            WorkingProfile::AdobeRgb => Profile::AdobeRgb,
            WorkingProfile::DisplayP3 => Profile::DisplayP3,
            WorkingProfile::ProPhoto => Profile::ProPhoto,
        }
    }
}

/// What `image.profile` does with the profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileAction {
    /// Read the pixels as being in `profile` (their numbers kept).
    Assign,
    /// Turn the pixels into `profile`, the colours kept.
    Convert,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProfileParams {
    /// Document id (see `doc.list`); the document shown when omitted.
    pub doc: Option<u32>,
    pub action: ProfileAction,
    pub profile: Profile,
}
impl Validate for ProfileParams {}

static PROFILE: CommandMeta = CommandMeta {
    id: "image.profile",
    label_vi: "Hồ sơ màu",
    label_en: "Color Profile",
    doc: "Image ▸ Color Profile: assign a working profile (the pixels are read in it, their \
          numbers kept) or convert to one (the colours kept). One undo step.",
    effects: &[Effect::Edit],
    featured: false,
};

pub struct ColorProfile;

impl Command for ColorProfile {
    type Params = ProfileParams;
    type Output = Moded;
    fn meta(&self) -> &'static CommandMeta {
        &PROFILE
    }
    fn plan(&self, s: &mut Session<'_>, p: ProfileParams) -> Result<Plan<Moded>, CmdError> {
        let doc = s.doc_id(p.doc)?;
        let canvas = s.canvas_mut(Some(doc))?;
        let profile = p.profile.core();
        let line = match p.action {
            ProfileAction::Assign => {
                canvas.assign_profile(profile);
                format!("Assigned profile: {}", profile.name())
            }
            ProfileAction::Convert => {
                canvas.convert_to_profile(profile);
                format!("Converted to profile: {}", profile.name())
            }
        };
        s.notice(line.clone(), line);
        s.emit(EngineEvent::Pixels { doc });
        moded(s, doc).map(Plan::Done)
    }
}
