// Persisted AI settings: the Gemini API key + recent prompt history.
// Stored next to the AI models, at %APPDATA%/IAI/ai.json (HOME/.local/share/iai
// on non-Windows) — same base dir convention as `lama.rs`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const MAX_HISTORY: usize = 20;

/// Which cloud image model the API path talks to.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiProvider {
    #[default]
    Gemini,
    OpenAi,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AiSettings {
    /// Google Gemini API key.
    #[serde(default)]
    pub api_key: String,
    /// OpenAI API key (gpt-image-1).
    #[serde(default)]
    pub openai_api_key: String,
    /// Which provider the API path uses.
    #[serde(default)]
    pub provider: AiProvider,
    #[serde(default)]
    pub history: Vec<String>,
}

impl AiSettings {
    /// The API key for the currently-selected provider.
    pub fn active_key(&self) -> &str {
        match self.provider {
            AiProvider::Gemini => &self.api_key,
            AiProvider::OpenAi => &self.openai_api_key,
        }
    }

    pub fn load() -> Self {
        std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create dir: {e}"))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&path, json).map_err(|e| format!("write: {e}"))
    }

    /// Push a prompt to the front of the MRU history (deduped, capped).
    pub fn add_history(&mut self, prompt: &str) {
        let p = prompt.trim().to_string();
        if p.is_empty() {
            return;
        }
        self.history.retain(|x| x != &p);
        self.history.insert(0, p);
        self.history.truncate(MAX_HISTORY);
    }
}

fn config_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("IAI")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local").join("share").join("iai")
    } else {
        PathBuf::from(".")
    }
}

fn config_path() -> PathBuf {
    config_dir().join("ai.json")
}
