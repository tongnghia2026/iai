// AI image editing via Google Gemini (`gemini-2.5-flash-image`, aka "nano-banana").
//
// The whole image (flattened Background) plus a text instruction is sent to the
// Gemini API; the returned image is resized back to the exact canvas size and
// dropped on top of the Background as a new layer, so the user can mask in only
// the parts they want. Mirrors the async pattern of `select_subject.rs`:
// `Arc<Mutex<Status>>` + a background thread + an mpsc channel polled per-frame.

pub mod edit;
pub mod retouch;
pub mod settings;

pub use settings::AiProvider;

/// Where the AI panel sends the image: a paid API (Gemini / OpenAI) or the free
/// browser-extension path driving the user's logged-in Gemini / ChatGPT tab.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum AiSource {
    #[default]
    GeminiApi,
    OpenAiApi,
    GeminiWeb,
    ChatGptWeb,
}

impl AiSource {
    pub fn is_web(self) -> bool {
        matches!(self, AiSource::GeminiWeb | AiSource::ChatGptWeb)
    }
    /// The API provider for API sources (`None` for the web/extension sources).
    pub fn api_provider(self) -> Option<AiProvider> {
        match self {
            AiSource::GeminiApi => Some(AiProvider::Gemini),
            AiSource::OpenAiApi => Some(AiProvider::OpenAi),
            _ => None,
        }
    }
    /// The site tag sent to the extension for web sources.
    pub fn web_site(self) -> Option<&'static str> {
        match self {
            AiSource::GeminiWeb => Some("gemini"),
            AiSource::ChatGptWeb => Some("chatgpt"),
            _ => None,
        }
    }
}

/// Editable UI buffers for the Gemini panel. Lives in `UiState` (persistent),
/// snapshotted into `UiData` each frame, edited locally in the panel and written
/// back via `UiActions::set_ai_panel` — same contract as `DevelopSettings`.
#[derive(Clone, PartialEq)]
pub struct AiPanelState {
    /// Gemini API key typed in the panel (mirror of the saved key on disk).
    pub api_key: String,
    /// OpenAI API key typed in the panel (mirror of the saved key on disk).
    pub openai_api_key: String,
    /// Which cloud provider the API path uses (Gemini / OpenAI).
    pub provider: AiProvider,
    /// Free-form prompt (manual mode).
    pub prompt: String,
    /// Short keyword for parametric presets (clothes / hair / background).
    pub param: String,
    pub retouch_preset: usize,
    pub color_preset: usize,
    pub light_preset: usize,
    pub sharpen_preset: usize,
    pub cleanup_preset: usize,
    pub bg_preset: usize,
    pub clothes_preset: usize,
    pub makeup_preset: usize,
    /// false = add result as a layer over Background, true = open as a new file.
    pub output_new_file: bool,
    /// false = paid API mode, true = free browser bridge (Gemini web session).
    /// Kept in sync with `source` (true ⇔ source.is_web()).
    pub use_bridge: bool,
    /// The selected source button (single source of truth for the UI). The legacy
    /// `use_bridge` + `provider` fields are derived from this in `source_selector`.
    pub source: AiSource,

    // ---- One-click ID-photo composer (Ảnh thẻ) ----
    /// 0 = không chỉ định, 1 = nam, 2 = nữ.
    pub id_gender: u8,
    /// 0 = trắng, 1 = xanh.
    pub id_bg: u8,
    /// Replace the outfit with formal ID-photo attire.
    pub id_change_clothes: bool,
    /// Outfit type when changing clothes: 0 = áo sơ mi, 1 = áo dài, 2 = vest.
    pub id_outfit: u8,
    /// Index into the shirt-color list (see SHIRT_COLORS in ai_panel.rs).
    pub id_shirt_color: u8,
    /// Index into the áo-dài-color list (see AODAI_COLORS in ai_panel.rs).
    pub id_aodai_color: u8,
    /// Index into the vest-color list (see VEST_COLORS in ai_panel.rs).
    pub id_vest_color: u8,
    /// Vest button count: 0 = one, 1 = two, 2 = three.
    pub id_vest_buttons: u8,
    /// Vest front style: 0 = single-breasted, 1 = double-breasted.
    pub id_vest_front: u8,
    /// Add a tie to the selected vest outfit.
    pub id_vest_tie: bool,
    /// Index into the tie-color list (see TIE_COLORS in ai_panel.rs).
    pub id_tie_color: u8,
    /// Tie pattern: 0 = plain, 1 = striped, 2 = subtle dotted.
    pub id_tie_pattern: u8,
    /// Tidy flyaway hair while preserving the original hairstyle.
    pub id_tidy_hair: bool,

    // ---- "Xếp ảnh in" sheet composer ----
    /// Cutting gap between placed photos, in pixels at sheet DPI.
    pub impose_gap_px: u32,

    /// Offline CPU/ONNX Auto Retouch controls. Kept with the existing AI panel
    /// state so the settings persist in the same UI snapshot contract.
    pub retouch: retouch::RetouchConfig,
}

impl Default for AiPanelState {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            openai_api_key: String::new(),
            provider: AiProvider::default(),
            prompt: String::new(),
            param: String::new(),
            retouch_preset: 0,
            color_preset: 0,
            light_preset: 0,
            sharpen_preset: 0,
            cleanup_preset: 0,
            bg_preset: 0,
            clothes_preset: 0,
            makeup_preset: 0,
            output_new_file: false,
            use_bridge: false,
            source: AiSource::default(),
            id_gender: 0,
            id_bg: 0,
            id_change_clothes: false,
            id_outfit: 0,
            id_shirt_color: 0,
            id_aodai_color: 0,
            id_vest_color: 0,
            id_vest_buttons: 1,
            id_vest_front: 0,
            id_vest_tie: false,
            id_tie_color: 0,
            id_tie_pattern: 0,
            id_tidy_hair: false,
            impose_gap_px: 10,
            retouch: retouch::RetouchConfig::default(),
        }
    }
}

pub fn guarded_edit_prompt(prompt: &str) -> String {
    format!(
        "Edit the attached image, not a new image. Preserve the exact original canvas size, aspect ratio, orientation, crop, framing, camera angle and perspective. Do not resize, extend, rotate, reframe, or change the image proportions. Preserve the original subject identity, facial features, face shape, expression, age, body proportions, pose, and natural skin texture. Do not beautify, reshape, replace, or alter the face unless explicitly requested. Keep the result aligned pixel-for-pixel to the original image so it can be used as a layer/mask over the source. Only change the requested attribute; keep all other details unchanged. Photorealistic result.\n\nUser edit instruction:\n{}",
        prompt.trim()
    )
}
