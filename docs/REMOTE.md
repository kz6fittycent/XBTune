# Remote tuning (phase 8): design for approval

Status: **approved by the owner 2026-10-15** with one change: the host can
let the tuner edit and burn directly (section 2.1), as pro remote-tuning
tools do. Section 1 records the answers.
Decisions already made by the owner (2026-10-15) are in CLAUDE.md
("Phase plan"). This document turns them into a protocol, a threat model
and a test plan, and lists the few choices still open (section 1).

The idea: the bike owner (the **host**) sits with the bike and XBTune; a
tuner somewhere else (the **guest**) runs XBTune too. The host reads a code
to the tuner over the phone. After that, the tuner sees the live data,
can open the tune, propose edits and ask for burns, tests and resets. By
default nothing reaches the ECM unless the host approves it in the same
dialog they use today; the host can instead give the tuner permission to
edit and burn directly (section 2.1).

## 1. Decisions (owner's answers, 2026-10-15)

1. **The code: a room number plus 6 digits.** A bare 6-digit code can't
   both find the session on the relay and stay secret from the relay: if
   the relay looks rooms up by the code (or a hash of it), whoever runs the
   relay can try all 1,000,000 codes offline and then sit in the middle.
   So the host is shown e.g. **`47-381926`**: `47` is a short room number
   the relay hands out (1-3 digits, only says *which* session), `381926`
   is the secret that never leaves the two apps. The tuner types all of
   it. *Recommended.* (Alternative: 6 digits only, split 2 + 4: shorter,
   but only 10,000 secrets.) **Owner: fine; keep it.**
2. **Accepting the tuner's edits.** Edits only change the host's working
   copy (never the ECM, never a file until the host saves), so two modes
   are offered, chosen by the host per session:
   - *Review each edit* (default): the tuner's edit shows on the host's
     map as a highlighted proposal; Accept / Reject (or Accept all).
   - *Apply the tuner's edits as they come*: still undoable, still
     journaled.
   **Owner: review-each as the default, but the host must also be able to
   let the tuner edit and burn directly** -> permissions, section 2.1.
3. **Saving a copy on the tuner's side.** The tune is the owner's. Default:
   the tuner may save a copy (needed to work offline later), with a host
   switch "Let the tuner save a copy of the tune" at session start.
   *Recommended: on by default, switch visible.* **Owner: yes.**
4. **A separate Worker** `xbtune-remote` (new `relay/remote/`), deployed
   by the owner like the bug-report relay, rather than adding WebSockets
   to the bug-report Worker. Keeps the bug-report relay small and lets
   either be turned off alone. *Recommended.* **Owner: yes.**
5. **Dependencies.** Approved: tokio-tungstenite, spake2, chacha20poly1305,
   hkdf, sha2, x25519-dalek. **x25519-dalek is not needed**: SPAKE2 already
   gives fresh keys per session (and per reconnect, section 4.4), so it's
   dropped. tokio-tungstenite needs its rustls feature with system roots
   (`rustls-native-certs`, already in the tree via ureq's verifier
   dependencies; licences re-checked with cargo-deny when added). If
   spake2 needs an RNG crate directly (`rand_core` with `getrandom`), I'll
   ask before adding it. **Owner: yes.**

## 2. What each side can do

| | Host (with the bike) | Guest (tuner) |
|---|---|---|
| Connect to the bike | yes, as today | no (its own bike link is off while joined) |
| Live data, dashboard, charts | yes | yes, the host's, as if connected |
| Record logs | yes | asks the host ("Record request") |
| Logs | shares chosen logs | opens shared logs in its Logs view |
| Open the tune | shares one tune (file or Read from ECM) | sees the shared working copy |
| Edit the tune | yes | proposes edits; edits directly with the *Edit* permission |
| Save files | host's files, on host's Save only | own copy, if allowed (section 1.3) |
| Burn | yes, with the usual review | **requests** it (host's dialog, pre-filled, "Requested by <tuner>"); with the *Burn* permission, reviews and confirms it itself (2.1) |
| Tests / resets / clear codes | yes | requests them; with the *Tests and resets* permission, runs them itself (2.1) |
| Unlock, Settings, folders | yes | never (no message exists for it) |
| Chat | yes | yes |

Without permissions the guest has no message that writes: a request only
opens the host's dialog, and the host's backup, review, confirmation,
engine/link checks and read-back all happen as for a local burn. The host
can end the session at any time; the guest's view then freezes with a
message ("The bike owner ended the session").

### 2.1 Permissions (host grants, per session)

Amends the earlier "the tuner can never write" (owner, 2026-10-15). Shown
as switches in the host's Remote panel, **all off at the start of every
session**, changeable at any time (taking one back applies at once):

1. **Edit the tune directly.** The tuner's edits apply to the host's
   working copy as they arrive (still journaled and undoable). Off =
   review each edit.
2. **Burn to the ECM.** Needs the host's own write unlock (Settings ->
   Safety locks) as today; the switch is disabled without it.
3. **Run tests and resets** (active tests, TPS/AFV reset, clear codes).
   Separate from burning: tests move things at the bike (fan, pump,
   injectors, coils), so the host decides about them on their own.

A burn, test or reset started by the tuner (KICKOFF §6 still applies in
full; only who confirms changes):
- runs in the **host's** session, on the host's link, with the host's
  automatic backup first, changed chunks only, read-back, and the
  engine-stopped and link-health checks; a failure offers Retry/Restore
  to both sides, and only the host can Restore without the permission;
- the **tuner** sees the full Burn review (diff in human units, big
  changes listed first, section 5) and confirms it;
- the **host's** screen then shows "<tuner> is about to burn N changes"
  with a **10-second countdown and a Stop button** before anything is
  written (tests: "... is about to run the fan test"); Stop cancels it;
- `burn.txt` in the history folder records "burned remotely by <tuner>,
  permission granted <time>"; app.log too.

The guest never holds the host's write permit: its burn message is a
command to the host's session, which checks the permission at that moment
(the same pattern as `Command::SetPermits` today).

## 3. Pieces

```
crates/xbtune-remote   protocol, crypto, relay client, session state machine (no UI)
crates/xbtune-app      remote_view.rs (Host / Join, chat, requests), hooks into Tune/Logs/Burn
crates/xbtune-cli      `remote host|join` (chat + live text) for testing without the GUI
relay/remote/          Cloudflare Worker + Durable Object (JavaScript, like relay/bug-report)
```

The relay is a dumb pipe: it pairs two WebSockets by room number and
forwards encrypted frames. It never sees the 6-digit secret, the keys or
any content.

## 4. Protocol

### 4.1 Relay (control messages, WebSocket text frames, JSON)

Endpoint: `wss://<relay>/v1/ws`. One Durable Object per room
(`idFromName(room)`); a small "rooms" Durable Object hands out free numbers.

| From | Message | Meaning |
|---|---|---|
| host | `{"t":"open","v":1}` | asks for a room |
| relay | `{"t":"opened","room":"47","ticket":"<32 random bytes, b64>"}` | room number + host's rejoin ticket |
| guest | `{"t":"join","v":1,"room":"47"}` | joins a room |
| relay | `{"t":"joined","ticket":"..."}` | to the guest: its rejoin ticket |
| relay | `{"t":"peer","state":"here"\|"gone"}` | to either side |
| either | `{"t":"rejoin","room":"47","ticket":"..."}` | after a dropped connection |
| either | `{"t":"close"}` | ends the room for both |
| relay | `{"t":"error","code":"...","text":"..."}` | e.g. `no-room`, `room-full`, `rate`, `version` |

Binary frames are forwarded unchanged to the other side, and only once
both are present (no store-and-forward; the apps resend, section 4.5).

Relay rules:
- Room numbers 1-999, random among free ones, so the next number can't be
  guessed. Exactly one host and one guest per room; a second `join` gets
  `room-full`.
- Rooms close 30 minutes after both sides are gone, after 12 hours
  in total, or on `close`. The host's app also closes the room when the
  key check fails (4.3).
- Rate limits per IP (Workers rate-limit binding): 10 `open` and 10
  `join` per minute; frames up to 1 MiB; 200 frames per second per side.
- No accounts, no stored content. Cloudflare's own request logs (IP,
  time) exist as for any Worker; the app says so on the first-use guide.

### 4.2 Pairing (SPAKE2)

`spake2` crate, Ed25519 group, asymmetric roles:
- password = the 6 digits as ASCII;
- identity A = `"xbtune-remote/1 host room=<room>"`, B = the same with
  `guest`, so a key can't be replayed into another room or role.

Each side sends its SPAKE2 message as the first binary frame:
`[0x01 version][0x00 kind=pake][32-byte message]`. Both get the same
32-byte `K` only if they typed the same digits; the relay learns nothing
from the exchange, and an attacker gets **one guess per pairing attempt**,
which they can't check offline.

### 4.3 Keys and key check

```
prk  = HKDF-SHA256-Extract(salt = "xbtune-remote/1", ikm = K)
k_hg = Expand(prk, "host->guest" || room || epoch, 32)
k_gh = Expand(prk, "guest->host" || room || epoch, 32)
k_rs = Expand(prk, "resume"      || room || epoch, 32)
```

The first encrypted frame each way is `Hello` (4.6), which proves the key.
If the host can't decrypt the guest's Hello (wrong digits, or someone
else's guess), it shows "The code didn't match", closes the room and
offers a new code; the next attempt needs a new room number and new
digits. A guesser has a 1-in-a-million chance per code the owner reads
out, and must also beat the real tuner to the room.

### 4.4 Reconnects (epochs)

If either WebSocket drops (Wi-Fi, laptop sleep), both sides `rejoin` the
room with their ticket within 30 minutes, then run SPAKE2 again with
`password = k_rs` of the last epoch and `epoch + 1`. No code to re-enter,
fresh keys each time, and the relay ticket alone is useless without the
previous keys. After 30 minutes the room is gone and the host reads out a
new code.

### 4.5 Encrypted frames

```
frame = [0x01 version][0x01 kind=data][8-byte counter, big-endian][ciphertext + 16-byte tag]
ChaCha20-Poly1305, key k_hg or k_gh, nonce = 4 zero bytes || counter,
AAD = version || kind || direction byte || room
```

Counters start at 0 per direction per epoch and only go up; a frame whose
counter isn't the next one is dropped and ends the epoch (forces a
rejoin). Any decryption failure ends the epoch. Plaintext is one JSON
message (`serde_json`, already approved), at most 1 MiB; bigger things
(logs) go in chunks.

### 4.6 Messages (inside the encryption)

All messages: `{"m": "<type>", ...}`. Unknown types are ignored, so either
side can be one version ahead. Both sides send `Hello` first:
`{"m":"hello","app":"0.1.0+git...","proto":1,"role":"host","name":"..."}`.
The name is what the other side sees ("Requested by Dave").

| Stage | Message | Direction | Content |
|---|---|---|---|
| R1 | `chat` | both | text, max 2,000 chars |
| R1 | `bye` | both | ending on purpose |
| R2 | `identity` | h -> g | ECM version, ID, definition ID, runtime length; guest needs the same data file |
| R2 | `frames` | h -> g | batch of raw runtime frames with times (every 250 ms, ~700 B/s); guest decodes them itself |
| R2 | `link` | h -> g | state (live / reconnecting / stopped), Hz, error rate |
| R3 | `tune` | h -> g | full image (`.xbt` bytes, base64) + `seq`, sent on share and on resync |
| R3 | `op` | g -> h | proposed edit: `{id, var, cells, op}` with the same ops as `Tune::apply_op` |
| R3 | `applied` | h -> g | host accepted: `{seq, op, by}`; every accepted edit, host's own included |
| R3 | `rejected` | h -> g | `{id, reason}` |
| R3 | `sync` | g -> h | "I have up to seq N" after a reconnect |
| R3 | `permissions` | h -> g | `{edit, burn, tests}` whenever they change |
| R4 | `request` | g -> h | `{id, kind: burn\|test\|reset\|clear_codes\|record, vars?, note?, test?, direct}`; `direct: true` = run under a permission, after the host's countdown |
| R4 | `burn_review` | h -> g | the host's prepared backup/diff for a direct burn, for the tuner's review |
| R4 | `request_state` | h -> g | `{id, state: shown\|declined\|countdown\|stopped\|running\|done\|failed, text}` |
| R5 | `log_offer` / `log_chunk` / `log_done` | h -> g | shared CSV log in 256 KB chunks with SHA-256 |

### 4.7 Shared editing

- The **host's working copy is the only truth**. Every edit that lands
  (host's own or an accepted guest edit) gets the next `seq` and is sent
  as `applied`; the guest's copy follows `applied` in order.
- **Journal:** the host appends every `applied` op to
  `~/XBTune/Remote/<date>_<room>/journal.jsonl` and flushes it before
  sending, together with the starting image (`start.xbt`). After a crash
  the host can replay it ("Recover the remote session's edits?").
- **Drops:** the guest keeps its last `seq`; after a rejoin it sends
  `sync`, and the host resends the missed `applied` ops (or the whole
  `tune` if more than 500 behind). Unconfirmed guest proposals are resent
  with the same `id` (the host ignores duplicates).
- **Undo** on the host undoes the last op (journaled as an op too).
- **Files:** the host's tune file changes only on the host's Save
  (temp + rename, as today). Burning uses the host's working copy and the
  normal Burn review. **Never auto-burn.**

### 4.8 Requests

A guest request opens the host's existing dialog (Burn, test confirm,
TPS/AFV reset, clear codes) pre-filled from the request, with a banner
"Requested by <name>: <note>". Everything after that is the local flow.
The guest sees the state and result. One request at a time; more are
queued and shown as a list the host can decline.

## 5. Threat model

With no permissions granted, the table below holds as written. With
permissions, a tuner (or someone who overheard the code and joined first)
can change the tune or burn it, so the host should grant them only after
seeing the joined name and, ideally, hearing the tuner on the phone. The
countdown with Stop, the automatic backup and Restore are the backstops.


| Who | Can | Can't (because) |
|---|---|---|
| Network attacker (Wi-Fi, ISP) | see that you talk to the relay, sizes, timing | read or change anything (TLS + end-to-end encryption) |
| Relay operator (the owner's Cloudflare account) or Cloudflare | see room numbers, IPs, timing, sizes; drop or delay frames | read content, learn the digits, take over a session (SPAKE2 has no offline guessing; one guess per pairing attempt) |
| Someone who learns the room number | try one guess, which closes the room | pair without the digits |
| Someone who overhears the code | join before the tuner, as "the tuner" | do anything the host doesn't approve; the host sees the joining name and must approve every write |
| A malicious or careless tuner | propose bad edits, request burns; **with permissions**, edit and burn (host can Stop during the countdown, Restore after) | unlock, change settings or files; burn without the Burn permission and the host's own unlock; skip the backup, the engine/link checks or the read-back |
| A malicious host | send nonsense to the tuner | crash the tuner: all inputs are size-limited and parsed defensively (fuzzed) |

Out of scope: a compromised laptop on either side; a host who approves a
harmful burn after reviewing it.

Social engineering is the realistic risk ("just burn it, trust me"), so
the Burn review for a requested burn adds: changes larger than 10% in any
fuel/spark cell, or any rev-limit/fan/safety-related value, are listed
first and marked.

## 6. Test plan

Automated (CI):
1. **Crypto:** SPAKE2 pairing succeeds with the same digits and fails with
   different ones; keys differ per room, role, epoch; tampered, replayed,
   reordered, truncated and wrong-key frames are rejected and end the
   epoch; counters never repeat.
2. **Relay protocol against a local relay:** a small in-process relay in
   Rust (test-only, same control messages) for: open/join/rejoin,
   room-full, expiry, rate errors, one-guess-then-closed.
3. **Session end-to-end** (two in-process sessions + simulated ECM):
   live frames arrive and decode; edits round-trip; drop the link mid-edit
   and check the journal, `sync` and final images match; a requested burn
   produces the host's review event and *no* write until the host's
   confirm; the guest has no path to `Command::Burn` (type-level: the
   guest's session never holds a `WritePermit`). With the Burn
   permission: the tuner's direct burn waits for the countdown, Stop
   cancels it with no write, revoking the permission during the countdown
   cancels it, and it's refused when the host's write unlock is off.
4. **Hostile input:** fuzz the message parser (random/oversized JSON,
   huge chunks, bad base64); the app must refuse, not crash.
5. **Worker:** `wrangler dev` + a Node test script run in CI for the
   control messages (same cases as 2).

Manual (owner, at each stage): desktop as guest, laptop at the bike as
host, over the deployed relay: pair with the right code; a wrong code
closes the room; switch the guest's Wi-Fi off and on mid-edit; host
quits mid-session and recovers the journal; a requested burn of one
harmless value (like the first real burn) and its restore.

## 7. Stages

| Stage | Delivers | Owner does |
|---|---|---|
| R1 | Worker + pairing + encrypted chat (CLI and app "Remote" view, first-use guide with "Don't show again") | deploy `relay/remote/` (README steps, like the bug-report relay); test chat desktop <-> laptop |
| R2 | live data on the guest (dashboard, charts, live data, map live point) | engine running, tuner watching |
| R3 | shared tune, proposals/accept, journal, resync | edit together, drop the Wi-Fi once |
| R4 | requests and permissions (direct edit/burn/tests with countdown) | one requested and one direct harmless burn, a Stop during the countdown, and a restore |
| R5 | log sharing | share a ride log, run Auto-tune on the guest |

Each stage ends with tests green, a snap, and the owner's check before the
next one starts. Remote tuning stays a private feature until the
`ecmdroid.toml` licence question is settled (phase 9), since a tuner needs
an XBTune build to join.
