// SPDX-License-Identifier: GPL-3.0-or-later
//! One remote session (host or guest) as a tokio task: connect to the
//! relay, pair (SPAKE2), check the key with `hello`, then exchange
//! messages; after a dropped connection, rejoin and pair again with the
//! resume secret (docs/REMOTE.md 4.2-4.4). The UI talks to it through
//! [`Command`]s and [`Event`]s, like `xbtune_core::session`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::code::{self, Code};
use crate::crypto::{self, Keys, Pairing, Role, Secret};
use crate::msg::{self, Msg};
use crate::relay::{Action, Conn, Ctrl, Incoming, RelayError};

#[derive(Debug, Clone)]
pub struct Config {
    /// Relay base URL, e.g. `wss://xbtune-remote.example.workers.dev/v1`.
    pub relay: String,
    /// Shown to the other side.
    pub name: String,
    /// App version, shown to the other side.
    pub app: String,
    /// How long to keep trying to get back after a dropped connection
    /// (the relay keeps an empty room for 30 minutes).
    pub retry_for: Duration,
    pub ping_every: Duration,
}

impl Config {
    pub fn new(relay: impl Into<String>, name: impl Into<String>, app: impl Into<String>) -> Self {
        Self {
            relay: relay.into(),
            name: name.into(),
            app: app.into(),
            retry_for: Duration::from_secs(29 * 60),
            ping_every: Duration::from_secs(25),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Start {
    Host,
    Join(Code),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Host: the code to read out to the tuner.
    Code(Code),
    /// Connected to the relay, waiting for the other side.
    Waiting,
    /// Paired and the key checked. `resumed`: after a dropped connection.
    Paired {
        peer: String,
        app: String,
        resumed: bool,
    },
    /// The other side's connection dropped; it has 30 minutes to return.
    PeerGone,
    Chat {
        mine: bool,
        text: String,
    },
    /// Our connection to the relay dropped; trying again.
    Reconnecting {
        reason: String,
    },
    /// The session is over.
    Ended {
        reason: String,
        by_peer: bool,
    },
}

#[derive(Debug, Clone)]
pub enum Command {
    Chat(String),
    End,
    /// Tests: drop the relay connection as an outage would (then rejoin).
    #[doc(hidden)]
    SimulateDrop,
}

pub type Notify = Arc<dyn Fn() + Send + Sync>;

pub struct Remote {
    pub events: mpsc::UnboundedReceiver<Event>,
    commands: mpsc::UnboundedSender<Command>,
    pub task: tokio::task::JoinHandle<()>,
}

impl Remote {
    pub fn send(&self, c: Command) {
        let _ = self.commands.send(c);
    }
}

/// Start a session. Must be called inside a tokio runtime.
pub fn start(cfg: Config, how: Start, notify: Option<Notify>) -> Remote {
    let (etx, erx) = mpsc::unbounded_channel();
    let (ctx, crx) = mpsc::unbounded_channel();
    let out = Out { tx: etx, notify };
    let task = tokio::spawn(async move {
        let mut w = Worker::new(cfg, how, out.clone(), crx);
        let (reason, by_peer) = w.run().await;
        info!(%reason, by_peer, "remote session ended");
        out.send(Event::Ended { reason, by_peer });
    });
    Remote { events: erx, commands: ctx, task }
}

#[derive(Clone)]
struct Out {
    tx: mpsc::UnboundedSender<Event>,
    notify: Option<Notify>,
}

impl Out {
    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        if let Some(n) = &self.notify {
            n();
        }
    }
}

/// How one relay connection ended.
enum ConnEnd {
    /// Over for good: (reason, ended by the other side).
    Done(String, bool),
    /// Dropped: rejoin.
    Lost(String),
}

struct Worker {
    cfg: Config,
    role: Role,
    out: Out,
    commands: mpsc::UnboundedReceiver<Command>,
    room: Option<String>,
    ticket: Option<String>,
    /// Passwords by epoch: the 6 digits for 0, then each committed epoch's
    /// resume secret. The last two are kept, so a side that missed the
    /// other's `hello` can still be met one epoch back.
    passwords: Vec<(u32, Secret)>,
    pending_join: Option<Code>,
    paired_once: bool,
}

/// Per-connection pairing state.
#[derive(Default)]
struct Link {
    pairing: Option<Pairing>,
    keys: Option<Keys>,
    hello: bool,
}

impl Worker {
    fn new(cfg: Config, how: Start, out: Out, commands: mpsc::UnboundedReceiver<Command>) -> Self {
        let (role, pending_join) = match how {
            Start::Host => (Role::Host, None),
            Start::Join(c) => (Role::Guest, Some(c)),
        };
        Self {
            cfg,
            role,
            out,
            commands,
            room: None,
            ticket: None,
            passwords: Vec::new(),
            pending_join,
            paired_once: false,
        }
    }

    async fn run(&mut self) -> (String, bool) {
        // First connection: open or join. Refusals end the session.
        let first = match &self.pending_join {
            None => Action::Open,
            Some(c) => Action::Join { room: c.room.clone() },
        };
        let mut conn = match Conn::connect(&self.cfg.relay, &first).await {
            Ok(c) => c,
            Err(e) => return (e.to_string(), false),
        };
        let mut lost_since: Option<Instant> = None;
        let mut delay = Duration::from_secs(1);
        loop {
            match self.connection(&mut conn).await {
                ConnEnd::Done(reason, by_peer) => {
                    conn.close().await;
                    return (reason, by_peer);
                }
                ConnEnd::Lost(reason) => {
                    warn!(%reason, "relay connection lost");
                    conn.close().await;
                    let since = *lost_since.get_or_insert_with(Instant::now);
                    let (Some(room), Some(ticket)) = (self.room.clone(), self.ticket.clone()) else {
                        return (format!("lost the relay before the session started: {reason}"), false);
                    };
                    self.out.send(Event::Reconnecting { reason });
                    loop {
                        if since.elapsed() > self.cfg.retry_for {
                            return ("couldn't get back to the relay for 30 minutes".into(), false);
                        }
                        if self.wait_or_end(delay).await {
                            return ("You ended the session.".into(), false);
                        }
                        delay = (delay * 2).min(Duration::from_secs(15));
                        let action =
                            Action::Rejoin { room: room.clone(), role: self.role, ticket: ticket.clone() };
                        match Conn::connect(&self.cfg.relay, &action).await {
                            Ok(c) => {
                                conn = c;
                                lost_since = None;
                                delay = Duration::from_secs(1);
                                break;
                            }
                            Err(RelayError::NoRoom(_)) => return ("the session has ended".into(), false),
                            Err(e) if e.is_transient() => {
                                self.out.send(Event::Reconnecting { reason: e.to_string() })
                            }
                            Err(e) => return (e.to_string(), false),
                        }
                    }
                }
            }
        }
    }

    /// Sleep, unless the user ends the session meanwhile (true).
    async fn wait_or_end(&mut self, d: Duration) -> bool {
        let sleep = tokio::time::sleep(d);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return false,
                c = self.commands.recv() => match c {
                    None | Some(Command::End) => return true,
                    Some(Command::Chat(_) | Command::SimulateDrop) => {}
                },
            }
        }
    }

    fn password(&self, epoch: u32) -> Option<&Secret> {
        self.passwords.iter().find(|(e, _)| *e == epoch).map(|(_, s)| s)
    }

    fn next_epoch(&self) -> u32 {
        self.passwords.last().map_or(0, |(e, _)| *e)
    }

    async fn start_pairing(
        &mut self,
        conn: &mut Conn,
        link: &mut Link,
        epoch: u32,
    ) -> Result<(), RelayError> {
        let room = self.room.clone().unwrap_or_default();
        let Some(pw) = self.password(epoch).cloned() else { return Ok(()) };
        let (p, frame) = Pairing::start(self.role, &room, epoch, &pw);
        *link = Link { pairing: Some(p), ..Default::default() };
        conn.send_binary(frame).await
    }

    async fn send_msg(&self, conn: &mut Conn, link: &mut Link, m: &Msg) -> Result<(), RelayError> {
        if let Some(k) = &mut link.keys {
            conn.send_binary(k.seal(&m.encode())).await?;
        }
        Ok(())
    }

    async fn connection(&mut self, conn: &mut Conn) -> ConnEnd {
        let mut link = Link::default();
        let mut ping = tokio::time::interval(self.cfg.ping_every);
        ping.tick().await;
        loop {
            let r = tokio::select! {
                i = conn.next() => self.incoming(conn, &mut link, i).await,
                c = self.commands.recv() => self.command(conn, &mut link, c).await,
                _ = ping.tick() => conn.ping().await.map(|_| None).map_err(|e| ConnEnd::Lost(e.to_string())),
            };
            match r {
                Ok(None) => {}
                Ok(Some(end)) | Err(end) => return end,
            }
        }
    }

    async fn command(
        &mut self,
        conn: &mut Conn,
        link: &mut Link,
        c: Option<Command>,
    ) -> Result<Option<ConnEnd>, ConnEnd> {
        match c {
            None | Some(Command::End) => {
                let _ = self.send_msg(conn, link, &Msg::Bye).await;
                let _ = conn.send_close_room().await;
                Ok(Some(ConnEnd::Done("You ended the session.".into(), false)))
            }
            Some(Command::SimulateDrop) => Err(ConnEnd::Lost("simulated outage".into())),
            Some(Command::Chat(text)) => {
                if link.hello {
                    let text = msg::clean_chat(&text);
                    self.send_msg(conn, link, &Msg::Chat { text: text.clone() })
                        .await
                        .map_err(|e| ConnEnd::Lost(e.to_string()))?;
                    self.out.send(Event::Chat { mine: true, text });
                }
                Ok(None)
            }
        }
    }

    async fn incoming(
        &mut self,
        conn: &mut Conn,
        link: &mut Link,
        i: Result<Incoming, RelayError>,
    ) -> Result<Option<ConnEnd>, ConnEnd> {
        let lost = |e: RelayError| ConnEnd::Lost(e.to_string());
        match i.map_err(lost)? {
            Incoming::Closed => Err(ConnEnd::Lost("the relay closed the connection".into())),
            Incoming::Ctrl(Ctrl::Opened { room, ticket })
                if self.role == Role::Host && self.room.is_none() =>
            {
                let digits = code::new_digits();
                self.passwords = vec![(0, Secret::digits(&digits))];
                info!(%room, "remote room opened");
                self.room = Some(room.clone());
                self.ticket = Some(ticket);
                self.out.send(Event::Code(Code { room, digits }));
                self.out.send(Event::Waiting);
                Ok(None)
            }
            Incoming::Ctrl(Ctrl::Joined { ticket }) if self.role == Role::Guest && self.room.is_none() => {
                let c = self.pending_join.take().expect("a guest starts with a code");
                self.passwords = vec![(0, Secret::digits(&c.digits))];
                self.room = Some(c.room);
                self.ticket = Some(ticket);
                self.out.send(Event::Waiting);
                Ok(None)
            }
            Incoming::Ctrl(Ctrl::Peer { state }) if state == "here" => {
                let e = self.next_epoch();
                self.start_pairing(conn, link, e).await.map_err(lost)?;
                Ok(None)
            }
            Incoming::Ctrl(Ctrl::Peer { .. }) => {
                *link = Link::default();
                if self.paired_once {
                    self.out.send(Event::PeerGone);
                }
                Ok(None)
            }
            Incoming::Ctrl(Ctrl::Error { code, text }) => {
                Ok(Some(ConnEnd::Done(format!("relay error {code}: {text}"), false)))
            }
            Incoming::Ctrl(_) => Ok(None),
            Incoming::Binary(frame) => match crypto::frame_kind(&frame) {
                Some(crypto::KIND_PAKE) => self.pake(conn, link, &frame).await,
                Some(crypto::KIND_DATA) => self.data(conn, link, &frame).await,
                _ => Ok(None),
            },
        }
    }

    async fn pake(
        &mut self,
        conn: &mut Conn,
        link: &mut Link,
        frame: &[u8],
    ) -> Result<Option<ConnEnd>, ConnEnd> {
        let lost = |e: RelayError| ConnEnd::Lost(e.to_string());
        let Ok((their_epoch, msg)) = crypto::parse_pake(frame) else { return Ok(None) };
        let mine = link.pairing.as_ref().map(|p| p.epoch);
        match mine {
            Some(e) if e == their_epoch => {}
            // They're one behind (missed our hello): meet them there.
            _ if self.password(their_epoch).is_some() && mine.is_none_or(|e| their_epoch < e) => {
                self.start_pairing(conn, link, their_epoch).await.map_err(lost)?;
            }
            // They're ahead; they'll come back to our epoch.
            _ => return Ok(None),
        }
        let p = link.pairing.take().expect("pairing just checked");
        match p.finish(msg) {
            Ok(k) => {
                link.keys = Some(k);
                link.hello = false;
                let hello = Msg::Hello {
                    app: self.cfg.app.clone(),
                    proto: msg::PROTO,
                    role: self.role.name().into(),
                    name: self.cfg.name.clone(),
                };
                self.send_msg(conn, link, &hello).await.map_err(lost)?;
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    async fn data(
        &mut self,
        conn: &mut Conn,
        link: &mut Link,
        frame: &[u8],
    ) -> Result<Option<ConnEnd>, ConnEnd> {
        let Some(keys) = &mut link.keys else { return Ok(None) };
        let epoch = keys.epoch;
        let plain = match keys.open(frame) {
            Ok(p) => p,
            Err(e) if !link.hello => {
                // The key check failed: different digits (or a stranger's
                // guess). One guess per room: close it.
                warn!(epoch, error = %e, "remote key check failed");
                let _ = conn.send_close_room().await;
                return Ok(Some(ConnEnd::Done(
                    if epoch == 0 {
                        "The code didn't match. Start a new session for a new code.".into()
                    } else {
                        "Couldn't resume the session securely. Start a new session.".into()
                    },
                    false,
                )));
            }
            Err(e) => return Err(ConnEnd::Lost(format!("bad frame from the other side: {e}"))),
        };
        let m = match Msg::decode(&plain) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "ignored a message");
                return Ok(None);
            }
        };
        match m {
            Msg::Hello { app, proto, role, name } => {
                if role != self.role.other().name() {
                    let _ = conn.send_close_room().await;
                    return Ok(Some(ConnEnd::Done("both sides chose the same role".into(), false)));
                }
                if proto != msg::PROTO {
                    let _ = conn.send_close_room().await;
                    return Ok(Some(ConnEnd::Done(
                        format!(
                            "the other XBTune speaks remote protocol {proto}, this one {}; update both",
                            msg::PROTO
                        ),
                        false,
                    )));
                }
                link.hello = true;
                // Commit this epoch: the next one's password is its resume secret.
                let next = (epoch + 1, keys.resume_secret());
                self.passwords.retain(|(e, _)| *e >= epoch);
                self.passwords.push(next);
                let resumed = self.paired_once;
                self.paired_once = true;
                info!(epoch, resumed, "remote paired");
                self.out.send(Event::Paired {
                    peer: msg::clean_name(&name),
                    app: msg::clean_name(&app),
                    resumed,
                });
                Ok(None)
            }
            _ if !link.hello => Ok(None),
            Msg::Chat { text } => {
                self.out.send(Event::Chat { mine: false, text: msg::clean_chat(&text) });
                Ok(None)
            }
            Msg::Bye => Ok(Some(ConnEnd::Done("The other side ended the session.".into(), true))),
            Msg::Unknown => Ok(None),
        }
    }
}
