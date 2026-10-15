// SPDX-License-Identifier: GPL-3.0-or-later
//! Remote sessions end to end over the local relay (docs/REMOTE.md 6.2).

use std::time::Duration;

use xbtune_remote::code::Code;
use xbtune_remote::crypto::Role;
use xbtune_remote::local_relay::LocalRelay;
use xbtune_remote::session::{self, Command, Config, Event, Remote, Start};

fn cfg(relay: &LocalRelay, name: &str) -> Config {
    let mut c = Config::new(relay.url.clone(), name, "test");
    c.retry_for = Duration::from_secs(20);
    c
}

async fn next(r: &mut Remote) -> Event {
    tokio::time::timeout(Duration::from_secs(10), r.events.recv())
        .await
        .expect("an event within 10 s")
        .expect("session still running")
}

/// Wait for an event matching `f`, skipping others.
async fn until(r: &mut Remote, f: impl Fn(&Event) -> bool) -> Event {
    loop {
        let e = next(r).await;
        if f(&e) {
            return e;
        }
    }
}

async fn host_code(h: &mut Remote) -> Code {
    match until(h, |e| matches!(e, Event::Code(_))).await {
        Event::Code(c) => c,
        _ => unreachable!(),
    }
}

async fn paired(r: &mut Remote) -> (String, bool) {
    match until(r, |e| matches!(e, Event::Paired { .. } | Event::Ended { .. })).await {
        Event::Paired { peer, resumed, .. } => (peer, resumed),
        e => panic!("{e:?}"),
    }
}

async fn ended(r: &mut Remote) -> (String, bool) {
    match until(r, |e| matches!(e, Event::Ended { .. })).await {
        Event::Ended { reason, by_peer } => (reason, by_peer),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn pair_chat_and_end() {
    let relay = LocalRelay::start("127.0.0.1:0").await.unwrap();
    let mut h = session::start(cfg(&relay, "Owner"), Start::Host, None);
    let code = host_code(&mut h).await;
    assert_eq!(code.digits.len(), 6);
    let mut g = session::start(cfg(&relay, "Tuner"), Start::Join(code), None);
    assert_eq!(paired(&mut h).await, ("Tuner".into(), false));
    assert_eq!(paired(&mut g).await, ("Owner".into(), false));

    h.send(Command::Chat("hello from the garage".into()));
    g.send(Command::Chat("hi, start the engine".into()));
    let got = until(&mut g, |e| matches!(e, Event::Chat { mine: false, .. })).await;
    assert_eq!(got, Event::Chat { mine: false, text: "hello from the garage".into() });
    let got = until(&mut h, |e| matches!(e, Event::Chat { mine: false, .. })).await;
    assert_eq!(got, Event::Chat { mine: false, text: "hi, start the engine".into() });

    h.send(Command::End);
    assert!(!ended(&mut h).await.1);
    let (reason, by_peer) = ended(&mut g).await;
    assert!(by_peer, "{reason}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(relay.rooms(), 0, "ending closes the room");
}

#[tokio::test]
async fn wrong_code_closes_the_room() {
    let relay = LocalRelay::start("127.0.0.1:0").await.unwrap();
    let mut h = session::start(cfg(&relay, "Owner"), Start::Host, None);
    let code = host_code(&mut h).await;
    let wrong = Code {
        room: code.room.clone(),
        digits: if code.digits == "000000" { "000001" } else { "000000" }.into(),
    };
    let mut g = session::start(cfg(&relay, "Guesser"), Start::Join(wrong), None);
    assert!(ended(&mut h).await.0.contains("didn't match"));
    assert!(ended(&mut g).await.0.contains("didn't match"));
    // One guess per room: the right code no longer finds the room.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut late = session::start(cfg(&relay, "Tuner"), Start::Join(code), None);
    let (reason, _) = ended(&mut late).await;
    assert!(reason.contains("no session with room"), "{reason}");
}

#[tokio::test]
async fn second_guest_and_unknown_room_are_refused() {
    let relay = LocalRelay::start("127.0.0.1:0").await.unwrap();
    let mut h = session::start(cfg(&relay, "Owner"), Start::Host, None);
    let code = host_code(&mut h).await;
    let mut g = session::start(cfg(&relay, "Tuner"), Start::Join(code.clone()), None);
    paired(&mut g).await;
    let mut other = session::start(cfg(&relay, "Other"), Start::Join(code.clone()), None);
    assert!(ended(&mut other).await.0.contains("already joined"));
    let nowhere =
        Code { room: if code.room == "999" { "998" } else { "999" }.into(), digits: "123456".into() };
    let mut lost = session::start(cfg(&relay, "Lost"), Start::Join(nowhere), None);
    assert!(ended(&mut lost).await.0.contains("no session"));
    // The real pair is unaffected.
    g.send(Command::Chat("still here".into()));
    until(&mut h, |e| matches!(e, Event::Chat { mine: false, .. })).await;
}

#[tokio::test]
async fn dropped_connections_resume_with_new_keys() {
    let relay = LocalRelay::start("127.0.0.1:0").await.unwrap();
    let mut h = session::start(cfg(&relay, "Owner"), Start::Host, None);
    let code = host_code(&mut h).await;
    let mut g = session::start(cfg(&relay, "Tuner"), Start::Join(code.clone()), None);
    paired(&mut h).await;
    paired(&mut g).await;
    for (who, round) in [(Role::Guest, 1), (Role::Host, 2), (Role::Guest, 3)] {
        relay.kick(&code.room, who);
        assert!(paired(&mut h).await.1, "round {round}");
        assert!(paired(&mut g).await.1, "round {round}");
        h.send(Command::Chat(format!("after drop {round}")));
        let got = until(&mut g, |e| matches!(e, Event::Chat { mine: false, .. })).await;
        assert_eq!(got, Event::Chat { mine: false, text: format!("after drop {round}") });
    }
    g.send(Command::End);
    assert!(ended(&mut h).await.1);
}

/// The guest misses the host's `hello` of a new epoch (link cut right
/// then): the host has moved on one epoch, the guest hasn't. They must
/// meet one epoch back instead of failing the key check.
#[tokio::test]
async fn a_side_one_epoch_behind_catches_up() {
    let relay = LocalRelay::start("127.0.0.1:0").await.unwrap();
    let mut h = session::start(cfg(&relay, "Owner"), Start::Host, None);
    let code = host_code(&mut h).await;
    let mut g = session::start(cfg(&relay, "Tuner"), Start::Join(code.clone()), None);
    paired(&mut h).await;
    paired(&mut g).await;
    // Next epoch: frames to the guest are the host's pake, then its hello.
    // Lose the hello and cut the guest off.
    relay.sabotage(&code.room, Role::Guest, 2);
    relay.kick(&code.room, Role::Host);
    // Host: pairs (epoch 1) when the guest's hello arrives.
    assert!(paired(&mut h).await.1);
    // Then the guest is back, one epoch behind; both pair again.
    assert!(paired(&mut h).await.1);
    assert!(paired(&mut g).await.1);
    g.send(Command::Chat("caught up".into()));
    let got = until(&mut h, |e| matches!(e, Event::Chat { mine: false, .. })).await;
    assert_eq!(got, Event::Chat { mine: false, text: "caught up".into() });
}

#[tokio::test]
async fn app_messages_pass_through_in_order() {
    use xbtune_remote::msg::{B64, FrameMsg, Msg};
    let relay = LocalRelay::start("127.0.0.1:0").await.unwrap();
    let mut h = session::start(cfg(&relay, "Owner"), Start::Host, None);
    let code = host_code(&mut h).await;
    let mut g = session::start(cfg(&relay, "Tuner"), Start::Join(code), None);
    paired(&mut h).await;
    paired(&mut g).await;
    for i in 0..50u8 {
        let m = Msg::Frames { frames: vec![FrameMsg { t: f64::from(i), raw: B64(vec![i; 107]) }] };
        h.send(Command::Send(Box::new(m)));
    }
    // A big one (a shared log chunk) too.
    let big = Msg::LogChunk { id: 1, index: 0, data: B64(vec![7; xbtune_remote::msg::LOG_CHUNK]) };
    h.send(Command::Send(Box::new(big.clone())));
    for i in 0..50u8 {
        match until(&mut g, |e| matches!(e, Event::Message(_))).await {
            Event::Message(Msg::Frames { frames }) => assert_eq!(frames[0].raw.0, vec![i; 107]),
            e => panic!("{e:?}"),
        }
    }
    assert_eq!(until(&mut g, |e| matches!(e, Event::Message(_))).await, Event::Message(big));
}
