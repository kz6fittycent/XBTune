// SPDX-License-Identifier: GPL-3.0-or-later
//! One request, one answer, for either adapter (docs/AI.md §2).
//!
//! [`prepare`] builds the exact body that [`send`] posts, so "What gets
//! sent" shows the real bytes. Requests are blocking: call them off the UI
//! thread. The API key goes in a header only, never in the body, a log or
//! an error message.
//!
//! Anthropic: raw HTTP (there is no official Rust SDK): `POST /v1/messages`
//! with `x-api-key` and `anthropic-version: 2023-06-01`; the answer is the
//! text blocks of `content`; `stop_reason: "refusal"` means the model
//! declined. On the current models that support it, server-side fallback
//! (`fallbacks: "default"`, beta `server-side-fallback-2026-07-01`) lets a
//! refused request be retried on another model by the API itself.

use std::time::Duration;

use serde_json::{Value, json};

use crate::config::{Adapter, AiConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

/// A request ready to send: what "What gets sent" shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub url: String,
    pub body: String,
    pub host: String,
    pub local: bool,
    /// Rough size in tokens (bytes / 4), for the preview.
    pub approx_tokens: usize,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reply {
    pub text: String,
    /// The model that answered, as the server reports it.
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Why it stopped (`end_turn`, `stop`, `max_tokens`, `length`, ...).
    pub stop: Option<String>,
}

impl Reply {
    /// The answer was cut off by the output limit.
    pub fn truncated(&self) -> bool {
        matches!(self.stop.as_deref(), Some("max_tokens" | "length"))
    }

    /// The server counted far fewer input tokens than were sent: it probably
    /// cut the request to fit its context window (Ollama's default does,
    /// silently). The estimate is bytes / 4, which overestimates JSON a
    /// little, so only a big gap counts.
    /// A count right at a common context size (2048, 4096, ...) is the
    /// other sign: the request filled the window exactly.
    pub fn input_cut(&self, p: &Prepared) -> bool {
        let Some(n) = self.input_tokens else { return false };
        let at_window =
            (11..=15).any(|k| (1u64 << k).abs_diff(n) <= 8) && n as f64 >= p.approx_tokens as f64 * 0.5;
        at_window || (n as f64) < p.approx_tokens as f64 * 0.6
    }
}

/// What to tell the user when [`Reply::input_cut`] is true.
pub const INPUT_CUT_HINT: &str = "The server seems to have cut the request to fit its context window, so the \
model may not have seen all the data or the instructions. Raise the server's context length (Ollama: set \
OLLAMA_CONTEXT_LENGTH=16384, or the context slider in LM Studio) and ask again.";

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AiError {
    #[error("AI advisor not set up: {0}")]
    NotConfigured(String),
    #[error("{host} answered HTTP {status}: {message}")]
    Http { host: String, status: u16, message: String },
    #[error("couldn't reach {host}: {message}")]
    Network { host: String, message: String },
    #[error("unexpected answer from {host}: {message}")]
    BadReply { host: String, message: String },
    #[error("the model declined to answer{0}")]
    Refused(String),
}

/// Models with server-side fallback on refusals (Claude API).
const FALLBACK_MODELS: &[&str] =
    &["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const ANTHROPIC_VERSION: &str = "2023-06-01";

fn base(cfg: &AiConfig) -> String {
    cfg.base_url.trim().trim_end_matches('/').to_string()
}

fn uses_fallback(cfg: &AiConfig) -> bool {
    cfg.adapter == Adapter::Anthropic && FALLBACK_MODELS.contains(&cfg.model.trim())
}

/// Build the request for `system` and the conversation so far.
pub fn prepare(cfg: &AiConfig, system: &str, turns: &[Turn]) -> Result<Prepared, AiError> {
    if let Some(m) = cfg.missing() {
        return Err(AiError::NotConfigured(m.into()));
    }
    let role = |r: Role| match r {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let model = cfg.model.trim().to_string();
    let (url, body) = match cfg.adapter {
        Adapter::OpenAi => {
            let mut messages = vec![json!({"role": "system", "content": system})];
            messages.extend(turns.iter().map(|t| json!({"role": role(t.role), "content": t.text})));
            (format!("{}/chat/completions", base(cfg)), json!({"model": model, "messages": messages}))
        }
        Adapter::Anthropic => {
            let messages: Vec<Value> =
                turns.iter().map(|t| json!({"role": role(t.role), "content": t.text})).collect();
            let mut b = json!({
                "model": model,
                "max_tokens": cfg.max_tokens,
                "system": system,
                "messages": messages,
            });
            if uses_fallback(cfg) {
                b["fallbacks"] = json!("default");
            }
            (format!("{}/v1/messages", base(cfg)), b)
        }
    };
    let body = serde_json::to_string_pretty(&body).expect("JSON values serialise");
    Ok(Prepared { url, approx_tokens: body.len() / 4, body, host: cfg.host(), local: cfg.is_local(), model })
}

pub(crate) fn agent(cfg: &AiConfig) -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    let _ = rustls::crypto::ring::default_provider().install_default();
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(cfg.timeout_s.max(1))))
        .http_status_as_error(false)
        .user_agent(format!("XBTune/{}", env!("CARGO_PKG_VERSION")))
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

fn with_auth<B>(cfg: &AiConfig, mut r: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
    let key = cfg.key();
    match cfg.adapter {
        Adapter::OpenAi => {
            if !key.is_empty() {
                r = r.header("Authorization", format!("Bearer {key}"));
            }
        }
        Adapter::Anthropic => {
            r = r.header("x-api-key", key).header("anthropic-version", ANTHROPIC_VERSION);
            if uses_fallback(cfg) {
                r = r.header("anthropic-beta", FALLBACK_BETA);
            }
        }
    }
    if !cfg.header_name.trim().is_empty() {
        r = r.header(cfg.header_name.trim(), cfg.header_value.trim());
    }
    r
}

/// The text of a failed response: the provider's own message if it has one.
fn error_message(status: u16, body: &str) -> String {
    let v: Option<Value> = serde_json::from_str(body).ok();
    let msg = v.as_ref().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("error").filter(|e| e.is_string()))
            .or_else(|| v.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let hint = match status {
        401 | 403 => " (check the API key)",
        404 => " (check the URL and the model name)",
        429 => " (rate limit or quota: wait, or check your plan)",
        _ => "",
    };
    let text = msg.unwrap_or_else(|| body.chars().take(300).collect::<String>().trim().to_string());
    format!("{text}{hint}")
}

fn read(
    cfg: &AiConfig,
    resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<String, AiError> {
    let host = cfg.host();
    let mut resp = resp.map_err(|e| AiError::Network { host: host.clone(), message: e.to_string() })?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| AiError::Network { host: host.clone(), message: e.to_string() })?;
    if !(200..300).contains(&status) {
        return Err(AiError::Http { host, status, message: error_message(status, &body) });
    }
    Ok(body)
}

/// Send a prepared request and wait for the answer (blocking).
pub fn send(cfg: &AiConfig, p: &Prepared) -> Result<Reply, AiError> {
    let r = with_auth(cfg, agent(cfg).post(&p.url)).content_type("application/json").send(&p.body);
    let body = read(cfg, r)?;
    parse_reply(cfg.adapter, &cfg.host(), &body)
}

/// The answer in a successful response body.
pub fn parse_reply(adapter: Adapter, host: &str, body: &str) -> Result<Reply, AiError> {
    let bad = |m: &str| AiError::BadReply { host: host.into(), message: m.into() };
    let v: Value = serde_json::from_str(body).map_err(|e| bad(&format!("not JSON ({e})")))?;
    let num = |p: &str| v.pointer(p).and_then(Value::as_u64);
    let s = |p: &str| v.pointer(p).and_then(Value::as_str).map(str::to_string);
    match adapter {
        Adapter::OpenAi => {
            let choice = v.pointer("/choices/0").ok_or_else(|| bad("no choices"))?;
            let text =
                choice.pointer("/message/content").and_then(Value::as_str).unwrap_or_default().to_string();
            let stop = choice.get("finish_reason").and_then(Value::as_str).map(str::to_string);
            if text.is_empty() && stop.as_deref() == Some("content_filter") {
                return Err(AiError::Refused(" (content filter)".into()));
            }
            Ok(Reply {
                text,
                model: s("/model"),
                input_tokens: num("/usage/prompt_tokens"),
                output_tokens: num("/usage/completion_tokens"),
                stop,
            })
        }
        Adapter::Anthropic => {
            let stop = s("/stop_reason");
            if stop.as_deref() == Some("refusal") {
                let why = s("/stop_details/category").map(|c| format!(" ({c})")).unwrap_or_default();
                return Err(AiError::Refused(why));
            }
            let blocks = v.get("content").and_then(Value::as_array).ok_or_else(|| bad("no content"))?;
            let text: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            Ok(Reply {
                text: text.join("\n\n"),
                model: s("/model"),
                input_tokens: num("/usage/input_tokens"),
                output_tokens: num("/usage/output_tokens"),
                stop,
            })
        }
    }
}

/// The models the server offers, sorted (not every server lists them).
pub fn list_models(cfg: &AiConfig) -> Result<Vec<String>, AiError> {
    if cfg.base_url.trim().is_empty() {
        return Err(AiError::NotConfigured("choose a provider (or enter its URL)".into()));
    }
    let url = match cfg.adapter {
        Adapter::OpenAi => format!("{}/models", base(cfg)),
        Adapter::Anthropic => format!("{}/v1/models?limit=1000", base(cfg)),
    };
    let body = read(cfg, with_auth(cfg, agent(cfg).get(&url)).call())?;
    parse_models(&body).ok_or_else(|| AiError::BadReply { host: cfg.host(), message: "no model list".into() })
}

/// `{"data": [{"id": ...}, ...]}` (both adapters; Ollama's /v1 too), or
/// `{"models": [{"name": ...}, ...]}` (some LocalAI/Ollama-style servers).
pub fn parse_models(body: &str) -> Option<Vec<String>> {
    let v: Value = serde_json::from_str(body).ok()?;
    let (list, key) = match v.get("data") {
        Some(d) => (d, "id"),
        None => (v.get("models")?, "name"),
    };
    let mut ids: Vec<String> = list
        .as_array()?
        .iter()
        .filter_map(|m| m.get(key).and_then(Value::as_str).map(str::to_string))
        .collect();
    ids.sort();
    Some(ids)
}

/// Check the settings with a tiny request. Returns what the model said.
pub fn test_connection(cfg: &AiConfig) -> Result<Reply, AiError> {
    let turns = [Turn { role: Role::User, text: "Reply with the single word OK.".into() }];
    let mut small = cfg.clone();
    small.max_tokens = small.max_tokens.min(2000);
    let p = prepare(&small, "You are a connection test. Answer in one word.", &turns)?;
    send(&small, &p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_a_request_cut_to_the_context_window() {
        let p = Prepared {
            url: String::new(),
            body: String::new(),
            host: String::new(),
            local: true,
            approx_tokens: 2331,
            model: String::new(),
        };
        let r = |n| Reply { input_tokens: Some(n), ..Default::default() };
        assert!(r(2050).input_cut(&p));
        assert!(r(1000).input_cut(&p));
        assert!(!r(3900).input_cut(&p));
        assert!(!Reply::default().input_cut(&p));
    }

    #[test]
    fn model_list_formats() {
        assert_eq!(parse_models(r#"{"data":[{"id":"b"},{"id":"a"}]}"#), Some(vec!["a".into(), "b".into()]));
        // As LocalAI answered on the owner's PC (2026-10-19).
        assert_eq!(
            parse_models(r#"{"models":[{"name":"gemma3-4b","model":"gemma3-4b"}]}"#),
            Some(vec!["gemma3-4b".into()])
        );
        assert_eq!(parse_models("{}"), None);
    }
}
