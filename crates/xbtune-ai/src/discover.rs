// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding model servers already running on this computer, for the setup
//! wizard (the approach of Desktop Scriptures' `ai_setup.py`: default ports
//! on localhost only, a short timeout, never the network).

use std::time::Duration;

use crate::client;
use crate::config::{Adapter, AiConfig, PRESETS};

/// How long each local probe may take.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// A server that answered, with its models (chat models first).
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
}

/// Names that mark an embedding model (not for chat): a name heuristic, as
/// most servers don't report what a model is for.
const EMBEDDING: &[&str] =
    &["embed", "bge-", "bge:", "e5-", "gte-", "minilm", "nomic", "arctic-embed", "rerank"];
/// Chat models made for something else: listed, never suggested first.
const SPECIALIST: &[&str] = &["code", "coder", "math", "vision", "-vl", "guard"];

pub fn is_embedding(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    EMBEDDING.iter().any(|x| m.contains(x))
}

pub fn is_specialist(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    SPECIALIST.iter().any(|x| m.contains(x))
}

/// Chat models only, general-purpose first (otherwise in the server's order).
pub fn chat_models(models: &[String]) -> Vec<String> {
    let mut v: Vec<String> = models.iter().filter(|m| !is_embedding(m)).cloned().collect();
    v.sort_by_key(|m| is_specialist(m));
    v
}

/// Probe every local preset's default address at once (blocking, about
/// [`PROBE_TIMEOUT`]); the servers that answered, in preset order.
pub fn scan() -> Vec<Found> {
    let mut seen = Vec::new();
    let targets: Vec<(String, String)> = PRESETS
        .iter()
        .filter(|p| p.local)
        .filter(|p| {
            // Two presets on one address (llama.cpp and LocalAI): probe once.
            let new = !seen.contains(&p.base_url);
            seen.push(p.base_url);
            new
        })
        .map(|p| (p.name.to_string(), p.base_url.to_string()))
        .collect();
    let handles: Vec<_> = targets
        .into_iter()
        .map(|(name, url)| {
            std::thread::spawn(move || {
                let cfg = AiConfig {
                    adapter: Adapter::OpenAi,
                    base_url: url.clone(),
                    timeout_s: 2,
                    ..Default::default()
                };
                client::list_models(&cfg).ok().map(|models| Found { name, base_url: url, models })
            })
        })
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_and_filters_models() {
        let m: Vec<String> =
            ["qwen3-coder:latest", "embeddinggemma:latest", "gemma3:12b", "nomic-embed-text"]
                .map(String::from)
                .to_vec();
        assert_eq!(chat_models(&m), vec!["gemma3:12b", "qwen3-coder:latest"]);
    }
}
