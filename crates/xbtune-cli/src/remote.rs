// SPDX-License-Identifier: GPL-3.0-or-later
//! `remote host|join|relay`: remote tuning from the terminal (docs/REMOTE.md).
//! Stage R1: pairing and encrypted chat. Type a line to send it, `/quit`
//! (or Ctrl+D) to end the session.

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use tokio::io::{AsyncBufReadExt, BufReader};
use xbtune_remote::session::{self, Command, Config, Event, Start};

#[derive(Subcommand)]
pub enum RemoteCmd {
    /// Start a session for a tuner to join; prints the code to read out.
    Host(Side),
    /// Join a session with the code the bike owner read out (e.g. 47-381926).
    Join {
        code: String,
        #[command(flatten)]
        side: Side,
    },
    /// Run a relay on this computer (plain ws://, for a local network or tests).
    Relay {
        #[arg(long, default_value = "127.0.0.1:8790")]
        listen: String,
    },
}

#[derive(Args)]
pub struct Side {
    /// Relay base URL (default: XBTUNE_REMOTE_URL or the XBTune relay).
    #[arg(long)]
    relay: Option<String>,
    /// Your name, shown to the other side.
    #[arg(long, default_value = "XBTune")]
    name: String,
}

pub async fn remote(cmd: RemoteCmd) -> Result<()> {
    let (side, how) = match cmd {
        RemoteCmd::Relay { listen } => {
            let r = xbtune_remote::local_relay::LocalRelay::start(&listen).await?;
            println!("Relay listening: {}", r.url);
            println!("Use: xbtune-cli remote host --relay {}", r.url);
            tokio::signal::ctrl_c().await?;
            return Ok(());
        }
        RemoteCmd::Host(side) => (side, Start::Host),
        RemoteCmd::Join { code, side } => {
            let c = xbtune_remote::code::parse(&code).map_err(|e| anyhow!(e))?;
            (side, Start::Join(c))
        }
    };
    let relay = side.relay.unwrap_or_else(xbtune_remote::relay_url);
    let cfg = Config::new(relay.clone(), side.name, format!("xbtune-cli {}", env!("CARGO_PKG_VERSION")));
    eprintln!("Relay: {relay}");
    let mut s = session::start(cfg, how, None);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdin_open = true;
    loop {
        tokio::select! {
            e = s.events.recv() => match e {
                None => return Ok(()),
                Some(Event::Code(c)) => println!("Code: {c}   (read it to the tuner)"),
                Some(Event::Waiting) => println!("Waiting for the other side..."),
                Some(Event::Paired { peer, app, resumed }) => {
                    if resumed {
                        println!("Back with {peer}.");
                    } else {
                        println!("Paired with {peer} ({app}). End-to-end encrypted. Type to chat, /quit to end.");
                    }
                }
                Some(Event::PeerGone) => println!("The other side dropped out; waiting for them (up to 30 minutes)..."),
                Some(Event::Chat { mine: false, text }) => println!("> {text}"),
                Some(Event::Chat { mine: true, .. }) => {}
                Some(Event::Reconnecting { reason }) => println!("Connection to the relay lost ({reason}); reconnecting..."),
                Some(Event::Ended { reason, .. }) => {
                    println!("Session ended: {reason}");
                    return Ok(());
                }
            },
            l = lines.next_line(), if stdin_open => match l {
                Ok(Some(l)) if l.trim() == "/quit" => s.send(Command::End),
                Ok(Some(l)) if !l.trim().is_empty() => s.send(Command::Chat(l)),
                Ok(Some(_)) => {}
                _ => {
                    stdin_open = false;
                    s.send(Command::End);
                }
            },
        }
    }
}
