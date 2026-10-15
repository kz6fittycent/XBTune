// SPDX-License-Identifier: GPL-3.0-or-later
//! A relay in Rust with the same rules as the Cloudflare Worker
//! (`relay/remote/worker.js`, docs/REMOTE.md 4.1), for tests and for
//! trying remote tuning on a local network without Cloudflare
//! (`xbtune-cli remote relay`). Plain `ws://` only.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

use crate::crypto::{MAX_FRAME, Role};
use crate::relay::TICKET_HEADER;

struct Slot {
    tx: mpsc::UnboundedSender<Message>,
    id: u64,
}

#[derive(Default)]
struct Room {
    host: Option<Slot>,
    guest: Option<Slot>,
    host_ticket: String,
    guest_ticket: Option<String>,
    /// Tests: lose the Nth binary frame to this role, then cut its link.
    sabotage: Option<(Role, usize)>,
}

impl Room {
    fn slot(&mut self, role: Role) -> &mut Option<Slot> {
        match role {
            Role::Host => &mut self.host,
            Role::Guest => &mut self.guest,
        }
    }
}

#[derive(Default)]
struct State {
    rooms: HashMap<String, Room>,
    next_id: u64,
}

type Shared = Arc<Mutex<State>>;

pub struct LocalRelay {
    /// Base URL for clients, e.g. `ws://127.0.0.1:40123/v1`.
    pub url: String,
    task: tokio::task::JoinHandle<()>,
    state: Shared,
}

impl Drop for LocalRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn ticket() -> String {
    let rng = rustls::crypto::ring::default_provider().secure_random;
    let mut b = [0u8; 16];
    rng.fill(&mut b).expect("the system random source works");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn ctrl(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

fn refuse(status: StatusCode, text: &str) -> ErrorResponse {
    let mut r = ErrorResponse::new(Some(text.to_string()));
    *r.status_mut() = status;
    r
}

/// What the upgrade request asked for, once accepted.
enum Accepted {
    Open { room: String, ticket: String },
    Join { room: String, ticket: String },
    Rejoin { room: String, role: Role },
}

fn decide(state: &Shared, req: &Request) -> Result<Accepted, (StatusCode, &'static str)> {
    let path = req.uri().path();
    let query: HashMap<&str, &str> =
        req.uri().query().unwrap_or("").split('&').filter_map(|kv| kv.split_once('=')).collect();
    let mut st = state.lock().expect("relay state lock");
    let room_arg = query.get("room").map(|s| s.to_string()).unwrap_or_default();
    match path {
        "/v1/open" => {
            let free: Vec<u32> = (1..=999).filter(|n| !st.rooms.contains_key(&n.to_string())).collect();
            if free.is_empty() {
                return Err((StatusCode::SERVICE_UNAVAILABLE, "no free rooms"));
            }
            let mut b = [0u8; 4];
            rustls::crypto::ring::default_provider().secure_random.fill(&mut b).expect("random");
            let room = free[u32::from_be_bytes(b) as usize % free.len()].to_string();
            let t = ticket();
            st.rooms.insert(room.clone(), Room { host_ticket: t.clone(), ..Default::default() });
            Ok(Accepted::Open { room, ticket: t })
        }
        "/v1/join" => {
            let Some(room) = st.rooms.get_mut(&room_arg) else {
                return Err((StatusCode::NOT_FOUND, "no-room"));
            };
            if room.guest_ticket.is_some() {
                return Err((StatusCode::CONFLICT, "room-full"));
            }
            let t = ticket();
            room.guest_ticket = Some(t.clone());
            Ok(Accepted::Join { room: room_arg, ticket: t })
        }
        "/v1/rejoin" => {
            let role = match query.get("role") {
                Some(&"host") => Role::Host,
                Some(&"guest") => Role::Guest,
                _ => return Err((StatusCode::BAD_REQUEST, "role")),
            };
            let Some(room) = st.rooms.get(&room_arg) else {
                return Err((StatusCode::NOT_FOUND, "no-room"));
            };
            let given = req.headers().get(TICKET_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
            let want = match role {
                Role::Host => Some(room.host_ticket.as_str()),
                Role::Guest => room.guest_ticket.as_deref(),
            };
            if want != Some(given) {
                return Err((StatusCode::FORBIDDEN, "ticket"));
            }
            Ok(Accepted::Rejoin { room: room_arg, role })
        }
        _ => Err((StatusCode::NOT_FOUND, "path")),
    }
}

impl LocalRelay {
    /// Listen on `addr` (e.g. `127.0.0.1:0`).
    pub async fn start(addr: &str) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let url = format!("ws://{}/v1", listener.local_addr()?);
        let state: Shared = Arc::default();
        let st = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = state.clone();
                tokio::spawn(async move {
                    let mut accepted = None;
                    // The callback's error type is tungstenite's (an HTTP response).
                    #[allow(clippy::result_large_err)]
                    let cb = |req: &Request, resp: Response| match decide(&state, req) {
                        Ok(a) => {
                            accepted = Some(a);
                            Ok(resp)
                        }
                        Err((status, text)) => Err(refuse(status, text)),
                    };
                    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                        .max_message_size(Some(MAX_FRAME + 1024));
                    let Ok(ws) =
                        tokio_tungstenite::accept_hdr_async_with_config(stream, cb, Some(config)).await
                    else {
                        return;
                    };
                    if let Some(a) = accepted {
                        serve(state, ws, a).await;
                    }
                });
            }
        });
        Ok(Self { url, task, state: st })
    }

    /// Tests: drop one side's connection, as a network outage would.
    pub fn kick(&self, room: &str, role: Role) {
        let mut st = self.state.lock().expect("relay state lock");
        if let Some(r) = st.rooms.get_mut(room)
            && let Some(s) = r.slot(role).as_ref()
        {
            let _ = s.tx.send(Message::Close(None));
        }
    }

    /// Tests: lose the `nth` binary frame (from 1) sent to `role` from now
    /// on, and cut that side's connection at that moment.
    pub fn sabotage(&self, room: &str, role: Role, nth: usize) {
        if let Some(r) = self.state.lock().expect("relay state lock").rooms.get_mut(room) {
            r.sabotage = Some((role, nth));
        }
    }

    /// Open rooms (tests).
    pub fn rooms(&self) -> usize {
        self.state.lock().expect("relay state lock").rooms.len()
    }
}

async fn serve(state: Shared, ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>, a: Accepted) {
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let close = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || close {
                break;
            }
        }
    });
    let (room, role) = {
        let mut st = state.lock().expect("relay state lock");
        st.next_id += 1;
        let id = st.next_id;
        let (room, role, hello) = match a {
            Accepted::Open { room, ticket } => (
                room.clone(),
                Role::Host,
                Some(ctrl(serde_json::json!({"t":"opened","room":room,"ticket":ticket}))),
            ),
            Accepted::Join { room, ticket } => {
                (room, Role::Guest, Some(ctrl(serde_json::json!({"t":"joined","ticket":ticket}))))
            }
            Accepted::Rejoin { room, role } => (room, role, None),
        };
        let Some(r) = st.rooms.get_mut(&room) else { return };
        if let Some(h) = hello {
            let _ = tx.send(h);
        }
        // A rejoin replaces a stale connection of the same role.
        if let Some(old) = r.slot(role).replace(Slot { tx: tx.clone(), id }) {
            let _ = old.tx.send(Message::Close(None));
        }
        if let Some(other) = r.slot(role.other()) {
            let _ = other.tx.send(ctrl(serde_json::json!({"t":"peer","state":"here"})));
            let _ = tx.send(ctrl(serde_json::json!({"t":"peer","state":"here"})));
        }
        (room, (role, id))
    };
    let (role, id) = role;
    while let Some(Ok(m)) = stream.next().await {
        match m {
            Message::Binary(b) => {
                let mut st = state.lock().expect("relay state lock");
                if let Some(r) = st.rooms.get_mut(&room) {
                    let to = role.other();
                    if let Some((who, n)) = &mut r.sabotage
                        && *who == to
                    {
                        *n -= 1;
                        if *n == 0 {
                            r.sabotage = None;
                            if let Some(other) = r.slot(to) {
                                let _ = other.tx.send(Message::Close(None));
                            }
                            continue;
                        }
                    }
                    if let Some(other) = r.slot(to) {
                        let _ = other.tx.send(Message::Binary(b));
                    }
                }
            }
            Message::Text(t) if t.as_str().contains(r#""t":"close""#) => {
                let mut st = state.lock().expect("relay state lock");
                if let Some(r) = st.rooms.remove(&room) {
                    for s in [r.host, r.guest].into_iter().flatten() {
                        let _ = s.tx.send(Message::Close(None));
                    }
                }
                break;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    // Gone: free the slot (unless a rejoin already replaced it) and tell
    // the other side. Empty rooms stay for rejoins; the Worker expires them.
    {
        let mut st = state.lock().expect("relay state lock");
        if let Some(r) = st.rooms.get_mut(&room)
            && r.slot(role).as_ref().is_some_and(|s| s.id == id)
        {
            *r.slot(role) = None;
            if let Some(other) = r.slot(role.other()) {
                let _ = other.tx.send(ctrl(serde_json::json!({"t":"peer","state":"gone"})));
            }
        }
    }
    let _ = tx.send(Message::Close(None));
    let _ = writer.await;
}
