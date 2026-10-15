# Remote tuning relay

A Cloudflare Worker (free plan, no card) that connects the bike owner's XBTune
("host") with a tuner's XBTune ("guest") for remote tuning (docs/REMOTE.md).
It pairs the two connections by a room number and passes their messages
along. Everything between the apps is end-to-end encrypted with a key made
from the 6 secret digits of the code (`47-381926`: `47` is the room, the
digits never reach the relay), so the relay can't read or change anything.
It stores only the room's two tickets, and only while the room exists. Not
part of the snap.

The app side is `crates/xbtune-remote` (it connects to
`wss://xbtune-remote.jimmy-tigert.workers.dev/v1`; `XBTUNE_REMOTE_URL`
overrides it).

## One-time setup

Same Cloudflare account as the bug-report relay (subdomain
`jimmy-tigert.workers.dev`; if it differs, change `DEFAULT_RELAY` in
`crates/xbtune-remote/src/lib.rs`). No secrets and no GitHub token are needed.

```bash
cd relay/remote
npx wrangler@4 deploy
```

(`npx wrangler@4 login` first if this computer isn't logged in.) The first
deploy creates the Durable Object class `Room`; the free plan allows it.

## Checking it

```bash
curl -s https://xbtune-remote.jimmy-tigert.workers.dev/
```

should answer `XBTune remote relay`. Then a real session from two
terminals (any two computers):

```bash
xbtune.cli remote host --name Owner        # prints "Code: 47-381926"
xbtune.cli remote join 47-381926 --name Tuner
```

Type a line in either to chat; `/quit` ends it. Logs: `npx wrangler@4 tail`
(shows rooms opening and closing, never content).

Against a local copy (no Cloudflare): `tools/test_remote_relay.sh` runs it
under `wrangler dev` and runs the Rust relay tests against it (CI does too).

## Limits

- 10 new sessions or joins per minute per IP address; rejoins after a
  dropped connection need the room's ticket and aren't limited.
- One host and one guest per room; a wrong code closes the room (the host
  starts a new session for a new code).
- A room closes when either side ends it, 30 minutes after both sides
  have gone, or 12 hours after it opened.
- Frames up to 1 MiB.
