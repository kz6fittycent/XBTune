// SPDX-License-Identifier: GPL-3.0-or-later
//! The same checks against a real relay, e.g. the Worker under
//! `wrangler dev` (tools/test_remote_relay.sh) or the deployed one:
//!   XBTUNE_TEST_RELAY=ws://127.0.0.1:8792/v1 cargo test -p xbtune-remote --test external_relay
//! Skipped when XBTUNE_TEST_RELAY isn't set.

use std::time::Duration;

use xbtune_remote::code::Code;
use xbtune_remote::session::{self, Command, Config, Event, Remote, Start};

fn relay() -> Option<String> {
    std::env::var("XBTUNE_TEST_RELAY").ok()
}

fn cfg(url: &str, name: &str) -> Config {
    let mut c = Config::new(url, name, "test");
    c.retry_for = Duration::from_secs(30);
    c
}

async fn until(r: &mut Remote, f: impl Fn(&Event) -> bool) -> Event {
    loop {
        let e = tokio::time::timeout(Duration::from_secs(20), r.events.recv())
            .await
            .expect("an event within 20 s")
            .expect("session still running");
        if f(&e) {
            return e;
        }
    }
}

async fn code(h: &mut Remote) -> Code {
    match until(h, |e| matches!(e, Event::Code(_) | Event::Ended { .. })).await {
        Event::Code(c) => c,
        e => panic!("{e:?}"),
    }
}

async fn paired(r: &mut Remote) -> bool {
    match until(r, |e| matches!(e, Event::Paired { .. } | Event::Ended { .. })).await {
        Event::Paired { resumed, .. } => resumed,
        e => panic!("{e:?}"),
    }
}

async fn ended(r: &mut Remote) -> (String, bool) {
    match until(r, |e| matches!(e, Event::Ended { .. })).await {
        Event::Ended { reason, by_peer } => (reason, by_peer),
        _ => unreachable!(),
    }
}

async fn chat_arrives(from: &Remote, to: &mut Remote, text: &str) {
    from.send(Command::Chat(text.into()));
    let got = until(to, |e| matches!(e, Event::Chat { mine: false, .. })).await;
    assert_eq!(got, Event::Chat { mine: false, text: text.into() });
}

#[tokio::test]
async fn relay_pairs_chats_resumes_and_ends() {
    let Some(url) = relay() else { return };
    let mut h = session::start(cfg(&url, "Owner"), Start::Host, None);
    let c = code(&mut h).await;
    let mut g = session::start(cfg(&url, "Tuner"), Start::Join(c.clone()), None);
    assert!(!paired(&mut h).await && !paired(&mut g).await);
    chat_arrives(&h, &mut g, "one").await;
    chat_arrives(&g, &mut h, "two").await;
    // A second guest is refused; the pair carries on.
    let mut other = session::start(cfg(&url, "Other"), Start::Join(c.clone()), None);
    assert!(ended(&mut other).await.0.contains("already joined"));
    // Outages on either side: rejoin, pair again, still talking.
    for (who, n) in [("guest", 1), ("host", 2)] {
        if who == "guest" {
            g.send(Command::SimulateDrop)
        } else {
            h.send(Command::SimulateDrop)
        }
        assert!(paired(&mut h).await, "{who} {n}");
        assert!(paired(&mut g).await, "{who} {n}");
        chat_arrives(&h, &mut g, &format!("after drop {n}")).await;
    }
    g.send(Command::End);
    assert!(ended(&mut h).await.1);
    // The room is gone.
    let mut late = session::start(cfg(&url, "Late"), Start::Join(c), None);
    assert!(ended(&mut late).await.0.contains("no session"));
}

#[tokio::test]
async fn relay_wrong_code_closes_the_room() {
    let Some(url) = relay() else { return };
    let mut h = session::start(cfg(&url, "Owner"), Start::Host, None);
    let c = code(&mut h).await;
    let wrong =
        Code { room: c.room.clone(), digits: if c.digits == "000000" { "000001" } else { "000000" }.into() };
    let mut g = session::start(cfg(&url, "Guesser"), Start::Join(wrong), None);
    assert!(ended(&mut h).await.0.contains("didn't match"));
    assert!(ended(&mut g).await.0.contains("didn't match"));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut late = session::start(cfg(&url, "Tuner"), Start::Join(c), None);
    assert!(ended(&mut late).await.0.contains("no session"));
}
