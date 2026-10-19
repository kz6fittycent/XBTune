// SPDX-License-Identifier: GPL-3.0-or-later
//! Both adapters against a local mock server: no network needed.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use xbtune_ai::client::{self, AiError, Role, Turn};
use xbtune_ai::config::{Adapter, AiConfig};

/// What the server saw.
struct Seen {
    line: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Serve one request with `status` and `reply`; `None` never answers.
fn serve(status: u16, reply: Option<&'static str>) -> (String, mpsc::Receiver<Seen>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            r.read_line(&mut h).unwrap();
            let h = h.trim_end().to_string();
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':').unwrap();
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        let len: usize = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .map_or(0, |(_, v)| v.parse().unwrap());
        let mut body = vec![0; len];
        r.read_exact(&mut body).unwrap();
        tx.send(Seen { line: line.trim().into(), headers, body: String::from_utf8(body).unwrap() }).unwrap();
        match reply {
            Some(b) => {
                let _ = write!(
                    s,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}",
                    b.len()
                );
            }
            None => thread::sleep(std::time::Duration::from_secs(5)),
        }
    });
    (url, rx)
}

fn cfg(adapter: Adapter, url: &str, model: &str) -> AiConfig {
    AiConfig {
        adapter,
        base_url: match adapter {
            Adapter::OpenAi => format!("{url}/v1"),
            Adapter::Anthropic => url.to_string(),
        },
        model: model.into(),
        api_key: "sk-secret-key-123456".into(),
        timeout_s: 2,
        ..Default::default()
    }
}

fn ask() -> Vec<Turn> {
    vec![Turn { role: Role::User, text: "Why is it lean at cruise?".into() }]
}

#[test]
fn openai_request_and_reply() {
    let (url, seen) = serve(
        200,
        Some(
            r#"{"model":"qwen3:14b","choices":[{"message":{"role":"assistant","content":"Load 30 looks lean."},"finish_reason":"stop"}],"usage":{"prompt_tokens":1200,"completion_tokens":40}}"#,
        ),
    );
    let c = cfg(Adapter::OpenAi, &url, "qwen3:14b");
    let p = client::prepare(&c, "system text", &ask()).unwrap();
    let r = client::send(&c, &p).unwrap();
    let s = seen.recv().unwrap();
    assert_eq!(s.line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(s.header("authorization"), Some("Bearer sk-secret-key-123456"));
    // What the preview showed is what was sent, and the key isn't in it.
    assert_eq!(s.body, p.body);
    assert!(!p.body.contains("sk-secret"));
    let v: serde_json::Value = serde_json::from_str(&s.body).unwrap();
    assert_eq!(v["messages"][0]["role"], "system");
    assert_eq!(v["messages"][1]["content"], "Why is it lean at cruise?");
    assert_eq!(r.text, "Load 30 looks lean.");
    assert_eq!((r.input_tokens, r.output_tokens), (Some(1200), Some(40)));
    assert!(p.local);
}

#[test]
fn anthropic_request_reply_and_fallback() {
    let (url, seen) = serve(
        200,
        Some(
            r#"{"model":"claude-opus-5-5","content":[{"type":"thinking","thinking":""},{"type":"text","text":"First."},{"type":"text","text":"Second."}],"stop_reason":"end_turn","usage":{"input_tokens":900,"output_tokens":20}}"#,
        ),
    );
    let c = cfg(Adapter::Anthropic, &url, "claude-opus-5-5");
    let p = client::prepare(&c, "system text", &ask()).unwrap();
    let r = client::send(&c, &p).unwrap();
    let s = seen.recv().unwrap();
    assert_eq!(s.line, "POST /v1/messages HTTP/1.1");
    assert_eq!(s.header("x-api-key"), Some("sk-secret-key-123456"));
    assert_eq!(s.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(s.header("anthropic-beta"), Some("server-side-fallback-2026-07-01"));
    let v: serde_json::Value = serde_json::from_str(&s.body).unwrap();
    assert_eq!(v["system"], "system text");
    assert_eq!(v["max_tokens"], 16000);
    assert_eq!(v["fallbacks"], "default");
    assert_eq!(r.text, "First.\n\nSecond.");
    // Other models get no fallback.
    let c = cfg(Adapter::Anthropic, &url, "claude-haiku-4-5");
    let p = client::prepare(&c, "s", &ask()).unwrap();
    assert!(!p.body.contains("fallbacks"));
}

#[test]
fn refusal_errors_and_timeouts() {
    let (url, _seen) = serve(
        200,
        Some(
            r#"{"content":[],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}}"#,
        ),
    );
    let c = cfg(Adapter::Anthropic, &url, "claude-opus-5-5");
    let p = client::prepare(&c, "s", &ask()).unwrap();
    assert_eq!(client::send(&c, &p), Err(AiError::Refused(" (cyber)".into())));

    let (url, _seen) = serve(
        401,
        Some(r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#),
    );
    let c = cfg(Adapter::Anthropic, &url, "claude-opus-5-5");
    let p = client::prepare(&c, "s", &ask()).unwrap();
    let e = client::send(&c, &p).unwrap_err();
    let text = e.to_string();
    assert!(
        text.contains("HTTP 401") && text.contains("invalid x-api-key") && text.contains("check the API key"),
        "{text}"
    );
    assert!(!text.contains("sk-secret"));

    let (url, _seen) = serve(200, None);
    let c = cfg(Adapter::OpenAi, &url, "m");
    let p = client::prepare(&c, "s", &ask()).unwrap();
    assert!(matches!(client::send(&c, &p), Err(AiError::Network { .. })));

    let (url, _seen) = serve(200, Some("<html>not json</html>"));
    let c = cfg(Adapter::OpenAi, &url, "m");
    let p = client::prepare(&c, "s", &ask()).unwrap();
    assert!(matches!(client::send(&c, &p), Err(AiError::BadReply { .. })));
}

#[test]
fn model_lists() {
    let (url, seen) =
        serve(200, Some(r#"{"data":[{"id":"llama3.1:8b"},{"id":"gemma3:12b"}],"object":"list"}"#));
    let c = cfg(Adapter::OpenAi, &url, "");
    assert_eq!(client::list_models(&c).unwrap(), vec!["gemma3:12b", "llama3.1:8b"]);
    assert_eq!(seen.recv().unwrap().line, "GET /v1/models HTTP/1.1");
    let (url, seen) = serve(
        200,
        Some(r#"{"data":[{"id":"claude-opus-5-5","display_name":"Claude Opus 5.5"}],"has_more":false}"#),
    );
    let c = cfg(Adapter::Anthropic, &url, "");
    assert_eq!(client::list_models(&c).unwrap(), vec!["claude-opus-5-5"]);
    assert_eq!(seen.recv().unwrap().line, "GET /v1/models?limit=1000 HTTP/1.1");
}

#[test]
fn not_configured_sends_nothing() {
    let c = AiConfig::default();
    assert!(matches!(client::prepare(&c, "s", &ask()), Err(AiError::NotConfigured(_))));
}
