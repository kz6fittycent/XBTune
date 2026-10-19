// SPDX-License-Identifier: GPL-3.0-or-later
//! Which model to ask and how: `<config>/ai.toml`, written with mode 0600
//! because it may hold an API key (docs/AI.md §2). `XBTUNE_AI_KEY`
//! overrides the stored key. The key is never logged: `Debug` hides it.

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The two request formats that reach almost every model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Adapter {
    /// `POST {base}/chat/completions`.
    #[default]
    #[serde(rename = "openai")]
    OpenAi,
    /// `POST {base}/v1/messages`.
    #[serde(rename = "anthropic")]
    Anthropic,
}

impl Adapter {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI-compatible",
            Self::Anthropic => "Anthropic Messages",
        }
    }
}

/// A provider with its adapter and base URL filled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub adapter: Adapter,
    pub base_url: &'static str,
    /// Runs on the user's computer (nothing leaves the machine).
    pub local: bool,
    pub note: &'static str,
}

const fn p(
    name: &'static str,
    adapter: Adapter,
    base_url: &'static str,
    local: bool,
    note: &'static str,
) -> Preset {
    Preset { name, adapter, base_url, local, note }
}

use Adapter::{Anthropic, OpenAi};

/// Every provider XBTune knows a URL for (checked 2026-10-19 against each
/// provider's documentation where noted in docs/AI.md). Local servers use
/// their default ports; change the URL if yours differs.
pub const PRESETS: &[Preset] = &[
    p(
        "Ollama",
        OpenAi,
        "http://localhost:11434/v1",
        true,
        "set OLLAMA_CONTEXT_LENGTH=16384 (the default is too small)",
    ),
    p(
        "Ubuntu inference snap (llama.cpp)",
        OpenAi,
        "http://127.0.0.1:8328/v1",
        true,
        "e.g. the gemma3 snap; other inference snaps may use other ports",
    ),
    p(
        "LM Studio",
        OpenAi,
        "http://localhost:1234/v1",
        true,
        "start the local server; load the model with 16k context",
    ),
    p("llama.cpp (llama-server)", OpenAi, "http://localhost:8080/v1", true, "default port 8080"),
    p("vLLM", OpenAi, "http://localhost:8000/v1", true, "default port 8000"),
    p("LocalAI", OpenAi, "http://localhost:8080/v1", true, "default port 8080"),
    p("Jan", OpenAi, "http://localhost:1337/v1", true, "check the port in Jan's local API server settings"),
    p("OpenAI", OpenAi, "https://api.openai.com/v1", false, ""),
    p("Anthropic (Claude)", Anthropic, "https://api.anthropic.com", false, ""),
    p(
        "Google Gemini",
        OpenAi,
        "https://generativelanguage.googleapis.com/v1beta/openai",
        false,
        "Gemini's OpenAI-compatible endpoint",
    ),
    p("OpenRouter", OpenAi, "https://openrouter.ai/api/v1", false, "many models behind one key"),
    p("Groq", OpenAi, "https://api.groq.com/openai/v1", false, ""),
    p("Mistral", OpenAi, "https://api.mistral.ai/v1", false, ""),
    p("DeepSeek", OpenAi, "https://api.deepseek.com", false, ""),
    p("xAI", OpenAi, "https://api.x.ai/v1", false, ""),
    p("Together AI", OpenAi, "https://api.together.ai/v1", false, ""),
    p("Fireworks AI", OpenAi, "https://api.fireworks.ai/inference/v1", false, ""),
    p("Perplexity", OpenAi, "https://api.perplexity.ai", false, ""),
    p("Cerebras", OpenAi, "https://api.cerebras.ai/v1", false, ""),
    p("Custom", OpenAi, "", false, "any OpenAI-compatible server (Azure OpenAI: add its api-key header)"),
];

pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Default request timeout: local models on a laptop can be slow.
pub const DEFAULT_TIMEOUT_S: u64 = 180;
/// Default output limit (Anthropic requires one; OpenAI-compatible servers
/// get none and use their own).
pub const DEFAULT_MAX_TOKENS: u32 = 16_000;

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// The preset this came from (for the UI), or "Custom".
    pub preset: String,
    pub adapter: Adapter,
    pub base_url: String,
    pub model: String,
    /// Empty for local servers that need none.
    pub api_key: String,
    /// An extra request header (e.g. Azure's `api-key`), if set.
    pub header_name: String,
    pub header_value: String,
    pub timeout_s: u64,
    pub max_tokens: u32,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            preset: String::new(),
            adapter: Adapter::OpenAi,
            base_url: String::new(),
            model: String::new(),
            api_key: String::new(),
            header_name: String::new(),
            header_value: String::new(),
            timeout_s: DEFAULT_TIMEOUT_S,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

impl fmt::Debug for AiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AiConfig")
            .field("preset", &self.preset)
            .field("adapter", &self.adapter)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &if self.api_key.is_empty() { "" } else { "(set)" })
            .field("header_name", &self.header_name)
            .field("timeout_s", &self.timeout_s)
            .field("max_tokens", &self.max_tokens)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}: {1}")]
    Parse(PathBuf, String),
}

impl AiConfig {
    /// Start from a preset (keeps model and key).
    pub fn apply_preset(&mut self, p: &Preset) {
        self.preset = p.name.to_string();
        self.adapter = p.adapter;
        if !p.base_url.is_empty() {
            self.base_url = p.base_url.to_string();
        }
    }

    /// `<config>/ai.toml`.
    pub fn path() -> Option<PathBuf> {
        xbtune_core::env::Dirs::detect().map(|d| d.config.join("ai.toml"))
    }

    /// Missing file: defaults (not set up).
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(t) => toml::from_str(&t).map_err(|e| ConfigError::Parse(path.into(), e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Io(path.into(), e)),
        }
    }

    /// Write with mode 0600 (temporary file, then rename).
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let io = |e| ConfigError::Io(path.into(), e);
        let text =
            toml::to_string_pretty(self).map_err(|e| ConfigError::Parse(path.into(), e.to_string()))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = path.with_extension("toml.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut f =
            std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(io)?;
        f.write_all(b"# XBTune AI advisor (docs/AI.md). Holds your API key: keep it private.\n")
            .map_err(io)?;
        f.write_all(text.as_bytes()).map_err(io)?;
        f.sync_all().map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)?;
        // An older file may have had wider permissions.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io)
    }

    /// The key to send: `XBTUNE_AI_KEY`, else the stored one.
    pub fn key(&self) -> String {
        std::env::var("XBTUNE_AI_KEY").ok().filter(|k| !k.is_empty()).unwrap_or_else(|| self.api_key.clone())
    }

    /// What's missing before a request can be sent, if anything.
    pub fn missing(&self) -> Option<&'static str> {
        if self.base_url.trim().is_empty() {
            Some("choose a provider (or enter its URL)")
        } else if self.model.trim().is_empty() {
            Some("enter a model name")
        } else if self.adapter == Adapter::Anthropic && self.key().is_empty() {
            Some("enter your API key")
        } else {
            None
        }
    }

    /// The server's host name, for "where it goes".
    pub fn host(&self) -> String {
        let rest = self.base_url.split("://").nth(1).unwrap_or(&self.base_url);
        let hostport = rest.split('/').next().unwrap_or_default();
        let host = match hostport.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or_default(),
            None => hostport.rsplit_once(':').map_or(hostport, |(h, _)| h),
        };
        host.to_ascii_lowercase()
    }

    /// The server runs on this computer.
    pub fn is_local(&self) -> bool {
        let h = self.host();
        h == "localhost" || h == "::1" || h.starts_with("127.") || h.ends_with(".localhost")
    }

    /// The key, if any, for masking it out of text (bug reports).
    pub fn secret(&self) -> Option<String> {
        let k = self.key();
        (k.len() >= 8).then_some(k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "xbtune-ai-cfg-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d.join("ai.toml")
    }

    #[test]
    fn saves_private_and_reads_back() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp();
        let mut c = AiConfig::default();
        c.apply_preset(preset("anthropic (claude)").unwrap());
        c.model = "claude-opus-5-5".into();
        c.api_key = "sk-test-0123456789".into();
        c.save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(AiConfig::load(&path).unwrap(), c);
        assert!(!format!("{c:?}").contains("0123456789"));
        assert_eq!(AiConfig::load(&path.with_file_name("none.toml")).unwrap(), AiConfig::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn hosts_and_locality() {
        let mut c = AiConfig { base_url: "http://localhost:11434/v1".into(), ..Default::default() };
        assert!(c.is_local());
        c.base_url = "http://[::1]:8080/v1".into();
        assert_eq!(c.host(), "::1");
        assert!(c.is_local());
        c.base_url = "https://API.openai.com/v1".into();
        assert_eq!(c.host(), "api.openai.com");
        assert!(!c.is_local());
    }

    #[test]
    fn presets_are_well_formed() {
        for p in PRESETS {
            if p.name != "Custom" {
                assert!(p.base_url.starts_with("http"), "{}", p.name);
                assert!(!p.base_url.ends_with('/'), "{}", p.name);
                let here =
                    p.base_url.starts_with("http://localhost") || p.base_url.starts_with("http://127.0.0.1");
                assert_eq!(p.local, here, "{}", p.name);
            }
        }
        assert_eq!(PRESETS.iter().filter(|p| p.adapter == Adapter::Anthropic).count(), 1);
    }

    #[test]
    fn missing_settings() {
        let mut c = AiConfig::default();
        assert!(c.missing().is_some());
        c.apply_preset(preset("Ollama").unwrap());
        assert_eq!(c.missing(), Some("enter a model name"));
        c.model = "qwen3:14b".into();
        assert_eq!(c.missing(), None);
    }
}
