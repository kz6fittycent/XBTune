// SPDX-License-Identifier: GPL-3.0-or-later
//! Talking to the relay (docs/REMOTE.md 4.1). The relay pairs two
//! WebSockets by room number and forwards binary frames between them; it
//! never sees keys or content.
//!
//! The action is in the URL, so the relay can route before the WebSocket
//! opens: `<base>/open`, `<base>/join?room=N`, `<base>/rejoin?room=N&role=R`
//! with the ticket in the `x-xbtune-ticket` header. Refusals are HTTP
//! statuses on the upgrade (404 no room, 409 room full, 429 rate limit,
//! 426 version). After that, the relay sends text frames ([`Ctrl`]) and
//! forwards binary frames.

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::crypto::{MAX_FRAME, Role};

pub const TICKET_HEADER: &str = "x-xbtune-ticket";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Open,
    Join { room: String },
    Rejoin { room: String, role: Role, ticket: String },
}

/// Text frames from the relay.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Ctrl {
    Opened {
        room: String,
        ticket: String,
    },
    Joined {
        ticket: String,
    },
    Peer {
        state: String,
    },
    Error {
        code: String,
        text: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug)]
pub enum Incoming {
    Ctrl(Ctrl),
    Binary(Vec<u8>),
    /// The relay closed the connection (room closed or expired).
    Closed,
}

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("no session with room {0}: check the code (or the session has ended)")]
    NoRoom(String),
    #[error("someone else already joined this session")]
    RoomFull,
    #[error("too many attempts from this address; wait a minute and try again")]
    RateLimited,
    #[error("the relay wants a newer XBTune")]
    Version,
    #[error("relay refused: HTTP {0}")]
    Http(u16),
    #[error("can't reach the relay: {0}")]
    Connect(String),
    #[error("relay connection: {0}")]
    Io(String),
}

impl RelayError {
    /// Worth retrying later (network trouble), as opposed to a refusal.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Connect(_) | Self::Io(_) | Self::Http(500..=599))
    }
}

pub struct Conn {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

fn url(base: &str, action: &Action) -> String {
    let base = base.trim_end_matches('/');
    match action {
        Action::Open => format!("{base}/open"),
        Action::Join { room } => format!("{base}/join?room={room}"),
        Action::Rejoin { room, role, .. } => format!("{base}/rejoin?room={room}&role={}", role.name()),
    }
}

impl Conn {
    pub async fn connect(base: &str, action: &Action) -> Result<Self, RelayError> {
        // tokio-tungstenite builds its rustls config with the process-wide
        // provider; XBTune uses ring (no-op if already installed).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut req =
            url(base, action).into_client_request().map_err(|e| RelayError::Connect(e.to_string()))?;
        if let Action::Rejoin { ticket, .. } = action {
            let v = HeaderValue::from_str(ticket).map_err(|e| RelayError::Connect(e.to_string()))?;
            req.headers_mut().insert(TICKET_HEADER, v);
        }
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME + 1024))
            .max_frame_size(Some(MAX_FRAME + 1024));
        let room = match action {
            Action::Join { room } | Action::Rejoin { room, .. } => room.clone(),
            Action::Open => String::new(),
        };
        match tokio_tungstenite::connect_async_with_config(req, Some(config), false).await {
            Ok((ws, _)) => Ok(Self { ws }),
            Err(tungstenite::Error::Http(resp)) => Err(match resp.status().as_u16() {
                404 => RelayError::NoRoom(room),
                409 => RelayError::RoomFull,
                429 => RelayError::RateLimited,
                426 => RelayError::Version,
                s => RelayError::Http(s),
            }),
            Err(e) => Err(RelayError::Connect(e.to_string())),
        }
    }

    pub async fn send_binary(&mut self, frame: Vec<u8>) -> Result<(), RelayError> {
        self.ws.send(Message::Binary(frame.into())).await.map_err(|e| RelayError::Io(e.to_string()))
    }

    /// Ask the relay to end the room for both sides.
    pub async fn send_close_room(&mut self) -> Result<(), RelayError> {
        self.ws
            .send(Message::Text(r#"{"t":"close"}"#.into()))
            .await
            .map_err(|e| RelayError::Io(e.to_string()))
    }

    pub async fn ping(&mut self) -> Result<(), RelayError> {
        self.ws.send(Message::Ping(Vec::new().into())).await.map_err(|e| RelayError::Io(e.to_string()))
    }

    pub async fn close(&mut self) {
        let _ = self.ws.close(None).await;
    }

    /// The next frame worth handling. Errors end the connection.
    pub async fn next(&mut self) -> Result<Incoming, RelayError> {
        loop {
            match self.ws.next().await {
                None => return Ok(Incoming::Closed),
                Some(Err(e)) => return Err(RelayError::Io(e.to_string())),
                Some(Ok(Message::Binary(b))) => return Ok(Incoming::Binary(b.to_vec())),
                Some(Ok(Message::Text(t))) => {
                    // Junk from the relay is ignored, not fatal.
                    if let Ok(c) = serde_json::from_str::<Ctrl>(t.as_str()) {
                        return Ok(Incoming::Ctrl(c));
                    }
                }
                Some(Ok(Message::Close(_))) => return Ok(Incoming::Closed),
                Some(Ok(_)) => {} // ping/pong
            }
        }
    }
}
