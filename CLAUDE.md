# XBTune: working notes

The brief is `docs/KICKOFF.md`. Read it first. Its rules on clean-room work
(§2), write safety (§6) and phases (§8) are non-negotiable.

## Status

- **Phase 1: done.** On-bike pass over BuellTooth 2026-10-14. The owner has
  no USB cable, so the USB path is untested on hardware (signed off without it).
- **Phase 2 (definitions + read): built 2026-10-14; on-bike pass 2026-10-14**
  (laptop, snap git 23ea31c): full dump in 10.5 s, verified, 0 re-reads,
  161/161 requests, 0 retries, RTT avg 65 ms; CRC-32 3b54974e;
  `diff` against EcmDroid's own save (`BUEIB_20261014-162837.xpr`) =
  "Identical (1210 bytes compared)". Fuel/spark maps decode sensibly.
  Owner's dumps and 26 Setup-screen values are in `tests/fixtures/owner/`;
  all golden tests pass.
- **Phase 3 (live data + logging + first app): built 2026-10-14; on-bike
  2026-10-14 "working quite well"** (snap a1bd3e7, engine running, logs in
  `tests/fixtures/owner/*.csv`, re-decoded by `owner_logs_decode_as_logged`).
  Good poll = ~146 ms (107 bytes at 9600 baud = 111 ms), so max ~6.8 Hz.
  With the engine running ~1 in 10 replies never arrived (owner suspects
  the BuellTooth); with the old 1 s timeout each loss cost 1.3 s -> 3.7 Hz.
  Live timeout is now 400 ms and failures are logged as `# event:` lines;
  next owner log should show the new rate and the failure pattern.
- **Phase 4 (diagnostics): built 2026-10-14; on-bike 2026-10-14** (snap
  9e6470a): `codes` read (no current; stored 15 "IAT high"); lock refused
  `test fan` until the OWNER created the unlock file on the laptop; then
  `test tachometer` (done after 4.4 s) and `test fan` (5.4 s) ran fine.
  EcmDroid also shows stored 15 (codes cross-checked). `codes --clear`
  on the bike cleared it. TPS reset deliberately not run (owner's choice).
  Phase 4 done.
- **Phase 6 (burn): built 2026-10-14; FIRST REAL BURN 2026-10-15** (laptop,
  snap 31eeddf, writes unlocked by the owner in Settings): KRPM_Hard_Hi
  7500 -> 7450. The two auto backups (16:39:49 / 16:41:38 UTC, both
  verified) differ in exactly 1 byte (0x01d2). Second burn 16:41:52 put
  7500 back. History folders checked: each burn 1 chunk/16 bytes on page 2,
  1 byte changed; ECM between burns == burn 1's after; final ECM ==
  original (all 1210 bytes). Round trip on the bike confirmed. The 11
  reconnects (owner's app.log): two outages of ~20 s, each starting ~10 s
  after a burn (16:40:27, 16:42:02), i.e. the owner switching the ignition
  off/on as the checklist says; then BlueZ returned EBUSY on RFCOMM connect
  until the old link timed out, and the app reconnected by itself. Not a
  fault. Since then: EBUSY gets its own message (no pairing advice), and
  session notes (burn steps, reconnects, failed requests) go to app.log. AFV reset not yet tried on the bike. Idea offered: burn.txt could
  list changed variables and "verified: yes".
  EEPROM writes are LOCKED on real hardware by their own key
  (`eeprom_writes = true` in hardware.toml), separate from `ecm_commands`.
  The owner unlocks it only after signing off on the simulator results:
  since 2026-10-15 via the app's Settings -> Safety locks (a switch plus a
  confirmation; the app writes hardware.toml on that click), or by hand.
  Never create or edit that file ourselves, and never click it in a test
  run against the real config (UI checks never write it; use XBTUNE_HOME).
- **Phase 5 (offline map editing): built 2026-10-14; owner tested
  2026-10-14** ("quite good"): edited a backup copy, set KRPM_Hard_Hi to
  7450 in it, and "Read from ECM" in the app worked on the bike. Feedback:
  the buttons took some getting used to (consider clearer labels/help).
  Live point checked on the bike at idle: load-20 row, between the 1350
  and 1000 RPM columns (consistent with a warm idle), so the RPM/Load
  pairing behaves sensibly. Engine-running link with the 400 ms timeout:
  5.6 Hz, 4% errors (was 3.7 Hz). Don't start phase 6 (burn) until the
  owner says so.
- **Phase 7 (log viewer + auto-tune): built 2026-10-15** (owner's go; stock
  narrowband). Sim/owner-log tested; NOT yet tried by the owner. See
  "Phase 7 contents" and "Phase 7 checklist".
- **Getting started + bug reports: built 2026-10-15** (owner asked; network
  feature OK'd by the owner per KICKOFF §9). Not yet on the laptop. The relay
  (`relay/bug-report/`) is NOT deployed yet: the owner does the Cloudflare/
  token steps in its README. Until then sends fail and reports queue in
  `<data>/app-log/pending/`.
- **Phase 8 remote tuning, stage R1 (relay + pairing + encrypted chat):
  built 2026-10-15** (REMOTE.md approved). `crates/xbtune-remote`: `code`
  (`47-381926`; digits from rustls/ring's secure random), `crypto`
  (SPAKE2 Ed25519 + HKDF-SHA256 + ChaCha20-Poly1305, counters, epochs),
  `msg` (hello/chat/bye, unknown ignored), `relay` (client; action in the
  URL, ticket header, HTTP refusals), `local_relay` (Rust relay with the
  Worker's rules; `kick`/`sabotage` test hooks), `session` (task: pair,
  key check = first `hello`, one wrong guess closes the room, rejoin +
  re-pair per epoch, a side one epoch behind is met there). Worker:
  `relay/remote/` (Durable Object per room, hibernation API, rate limit
  10/min/IP, rooms expire 30 min empty / 12 h) - NOT deployed yet: owner
  runs `npx wrangler@4 deploy` there (README). Default URL
  `wss://xbtune-remote.jimmy-tigert.workers.dev/v1`, `XBTUNE_REMOTE_URL`
  overrides. CLI `remote host|join|relay`. App: Remote view
  (`remote_view.rs`): name, Start session / Join, big code, status, chat,
  End; first-use guide ("Don't show this again" -> settings
  `remote_guide_seen`); session ended on app exit. Tests: crypto (5),
  code/msg, 5 end-to-end over the local relay (incl. 3 resumes and the
  epoch catch-up), 2 against the Worker under `wrangler dev`
  (`tools/test_remote_relay.sh`, CI job `remote-relay`). Hooks:
  `XBTUNE_SHOT_REMOTE=host|guide|join:<code>`, `XBTUNE_SHOT_REMOTE_CHAT`.
  Snap: `network` on the GUI, `network` + `network-bind` on the CLI.
  Owner check 2026-10-15 (relay deployed, snap 0500293, laptop <->
  desktop): connected twice, wrong code closed the session as intended.
  R1 PASSED. Owner expected remote AFV reset/tests already (that's R4);
  owner: keep the R2-R5 order and continue.
- **Remote R2-R5: built 2026-10-15** (sim-tested with two app instances;
  NOT yet tried by the owner). App logic `remote_sync.rs`, UI
  `remote_view.rs`; session passes app messages through
  (`Command::Send`/`Event::Message`; msg.rs has all R2-R5 types, binary
  as base64). R2: host forwards raw runtime frames every 250 ms +
  identity + link info; tuner's views show the host's bike
  (`LinkView.remote`, status "Remote · <owner>'s bike"); tuner can't
  connect a local bike while joined. R3: host shares the open tune (new
  tune = new share); edits sync as absolute byte patches found by diffing
  a snapshot every frame (any edit path works); tuner's edits are
  proposals (host Remote panel: Accept/Reject, Tune view banner) or
  applied at once with the Edit permission (`Tune::apply_patches`, one
  undo step "Tuner: ..."); tuner's copy = host truth + pending proposals
  (`Tune::replace_bytes`); after a re-pair the host resends the whole
  tune and the tuner resends pending proposals; tuner edits made while
  disconnected are captured first (`guest_capture`). Journal:
  `~/XBTune/Remote/<stamp>_room<N>/{start.xbt,journal.jsonl}`, Remote view
  "Last remote session: Open that tune". Saving a copy blocked when the
  host turns it off (Save/Save as/Ctrl+S/recovery file; tune closed at
  session end). R4: tuner's Run/Burn/AFV become requests; without
  permission the host gets a prompt (Review.../Decline/Later) that opens
  the normal dialog; with permission: function -> 10 s countdown (Stop)
  -> runs; burn/AFV -> host PrepareBurn (backup) -> review lines to the
  tuner -> tuner confirms -> countdown -> `burn_view::confirm_remote`
  with note "burned remotely by <tuner> (permission granted HH:MM)";
  revoking a permission stops a countdown; results go back as
  `request_state`. Record requests: Allow/Decline. R5: Logs view "Send to
  tuner" (192 KB chunks, SHA-256 checked) -> tuner's
  `~/XBTune/Remote/Logs/`, opened in Logs. Script hook for UI checks:
  `XBTUNE_SHOT_SCRIPT` (steps in `remote_view::set_script`),
  `XBTUNE_SHOT_DEMO=1`, code printed as `XBTUNE_REMOTE_CODE=` in
  screenshot mode only. Scenarios run (two instances, local relay,
  demo ECM): live dashboard, proposal review/accept + journal, direct
  burn with countdown (burn.txt note), Stop, request without permission,
  reviewed test, direct test, AFV, outage mid-edit (3 edits incl. one
  made while down), no-save tuner, log share (byte-identical).
- **Factory race ECMs: built 2026-10-15** (owner asked after seeing
  EcmDroid's race support). EcmDroid has no commits after our pin
  `2f17d29f`; its race support is `3c189be` (protocol choice, address
  0x55, "wrong Protocol?") + `2f17d29` (USB 19200; engraved = factory
  race, printed = P&A race = stock). XBTune: `SavedTarget` has `race`
  (serde default false; `Demo { race }`), Connect view "ECM type" radio,
  serial opens at `EcmAddress::baud()`, `session::wrong_type_hint` on
  version timeouts (session + CLI), `Simulator::b2rib_running()` (race
  demo; CLI `--sim --race`; pty `xbtune-sim --ecm B2RIB`). Tests: session
  read/live/burn on the race sim, CLI info. Hook `XBTUNE_SHOT_RACE=1`.
  Untested on a real race ECM (none available). BT adapters must be set
  to 19200 for factory race; XBTune can't change a BuellTooth's baud.
- **2026-10-15 evening owner test (snap 2aad16b):** warm ride logged
  (`tests/fixtures/owner/BUEIB_20261015-200748.csv`); auto-tune in the
  app proposed 8 cells (4/map, ~-3 units at load 20 idle and load 30
  2900-3400 rpm, EGO ~95%), CLI agrees. Ride link: 3.2 Hz, 192 timeouts,
  50 NAKs (0x15), 10 corrupt; garage logs had no NAKs, so likely
  garbled requests on the move (one lost frame each; left as is). O2
  lean swings reach 0.00 V: they were counted as "sensor error" (28%);
  now only > 1.10 V is an error. "Factory race" didn't stop the
  connection: most likely "Last used" ignored the ECM type (fixed: ECM
  type card is first and applies to Last used; the session label/log
  `target:` line says "factory race ECM (0x55)"; a street ECM answering
  on 0x55 gets a visible note). Also: a never-answered link stops after
  `UNANSWERED_LIMIT` (3) silent version requests with the hint as an
  error message (it retried forever with the hint hidden); NAKs and
  garbled replies don't count as silence (`TransactionError::is_silence`).
  Owner retest 2026-10-15 (snap 0b5d69c): Factory race on the BUEIB
  stopped with the red message; Stock connected. Works on hardware (so
  the BUEIB does NOT answer on 0x55, as EcmDroid implies).
- **Tube-frame DDFI ECMs: built 2026-10-15** (owner moved them up from
  phase 9: "add the tuber features too for this phase"). Data regenerated
  from ecmdroid.db at the pinned commit (fresh clone; extractor reproduced
  the old toml byte for byte before `IN_SCOPE` gained "DDFI"; only
  additions). EcmDroid's Java has no DDFI-1 protocol/page differences
  (only `Bin2MslConverter`, binary logs, which we don't read). Changes:
  `Family::in_scope` includes Ddfi1; `tps_reset_supported` = not DDFI-3
  (as EcmDroid); 2-byte Values with elemsize 0 are one number (all of
  BUEGC; deviation in PROVENANCE); stored codes skip a missing HDiag<n>
  (BUEGC has no HDiag0). Fitment: Revision Moto says only "Tube frame
  1999-2002". Tests: session end-to-end for all 4 IDs (identify, live,
  read, codes, AFV reset, burn), defs catalogue test. UI check hook
  `XBTUNE_SHOT_SERIAL=<pty>` (+ `xbtune-sim --ecm BUEGC`). Untested on a
  real tube ECM (none available).
- Owner's bike: **2005 Buell XB9SX**, ECM **BUEIB** (DDFI-2), version string
  `BUEIB310 12-11-03`, BuellTooth `00:11:22:33:44:55` (RFCOMM channel 1).
- **Hardware tests run on a separate laptop** (the desktop can't reach the
  garage). Build the snap here, or (since 2026-10-15) take it from the
  "🧪 Snap Builds" run for the pushed commit: artifact named after the
  `.snap`, kept 14 days (`gh run download <run> -R kz6fittycent/XBTune`).
  The user installs it on the laptop with `sudo snap install --dangerous`. Interfaces must be
  connected on the laptop too. Can't run commands on the laptop: give the
  user exact commands and ask for the output.
- Phase 1 hardware result (laptop, snap git b5fa645): `info` OK, 23/23
  requests, 0 retries, RTT avg 65 ms (min 50, max 147).
- Snap confinement checks on this PC: see "Snap notes" below.
- Remote: private GitHub repo `git@github.com:kz6fittycent/XBTune.git`
  (origin, added 2026-10-15; owner OK'd pushing). Keep it private.
  Don't publish to the store. The snap name `xbtune` is registered (owner,
  2026-10-14); registration only, nothing uploaded.

### Phase 7 contents

- `xbtune-core::logfile`: reads `xbtune-log 1` CSVs back (time_s + raw
  frame re-decoded with the current definition; events kept).
- `xbtune-core::autotune`: `channel_for_axis` (RPM/Load), `frac_index`,
  `cell_weights` (bilinear), `analyze` (filters: CLT >= 80 C, closed loop =
  EGO changed in the last 3 samples, |dRPM/dt| <= 800/s, |dTPD/dt| <= 10/s,
  no Accel/Decel/WOT correction, dt <= 1 s; weighted EGO per cell),
  `propose` (new = old x EGO/100, min hits 30, cap +-10%, map rounding),
  `apply` (one undo step), `fuel_maps`.
- Evidence from the owner's logs (tests pin it): veCurr1 == fuel map
  interpolated at (RPM, Load) for 70% of samples within 0.15 (median 0.03);
  EGO holds at AFV in open loop (64 s at warm-up), moves every frame in
  closed loop; EGO starts from AFV, so it includes AFV -> advise AFV reset
  after burning corrections. One O2 sensor (rear); ECM's EGO applies to
  both cylinders -> both maps corrected by default (Front/Rear ticks).
- `xbtune-core::logstats`: O2 rich/lean/error % (split at KO2_Midpoint),
  avg rich/lean V; anomalies (gap > 2 s, CLT jump > 3.5 C, battery 11-16 V).
- CLI: `logstats LOG...`, `autotune --tune T LOG... [-o|--in-place]
  [--maps --min-hits --max-change --min-clt]` (saves only with -o).
- App: Logs view (`logs_view.rs`): open logs, stats, anomalies, charts
  with playhead, playback (only when not connected) driving Dashboard/Live
  data/Charts/map live point. Tune view: fuel-map overlays (samples per
  cell, average EGO) and Auto-tune dialog (`autotune_view.rs`).
- Not done (later / needs owner input): wideband input + lambda maps,
  "multiply maps by AFV then reset" helper, binary/.msl log formats.
- Hooks: `XBTUNE_SHOT_LOGS=a.csv:b.csv XBTUNE_SHOT_PLAY=<s>
  XBTUNE_SHOT_OVERLAY=hits|ego XBTUNE_SHOT_AUTOTUNE=1`.

### Getting started + bug reports

- `xbtune-app::wizard`: 6-step modal (welcome/safety, adapter + snap plugs,
  connecting, demo, back up first, reporting). Shown at start while
  `settings.show_wizard` (default true); "Don't show this at startup" box on
  every step; Finish turns it off; side bar "Getting started" reopens it.
- `xbtune-app::report`: tracing to `<data>/app-log/app.log` (+ `.1`, 2 MB
  rotation) besides stderr; panic hook -> `crash.json` (first panic of a
  session; summary/fingerprint from the innermost XBTune backtrace frame,
  FNV-1a, no line numbers); SIGSEGV/BUS/ILL/FPE/ABRT handler writes a line to
  `fatal.log` then chains to the previous handler (Rust's stack-overflow one).
  Reports: `build` (masked: /home/<user> -> ~, BT MACs), `send` (ureq on a
  thread; failure -> `pending/`, max 5, 30 days; retried 15 s after start).
- `report_view`: Report a problem (optional text + contact, "What gets sent"
  preview), crash offer at the next start or at once if the app survived
  (background panic). Nothing is sent without a click; UI checks never send.
- Relay: copy of Desktop Scriptures' Cloudflare Worker (`relay/bug-report/`,
  repo kz6fittycent/XBTune, label `user-report`, extra `ecm` field) and
  `.github/workflows/close-stale-reports.yml`.
- Hooks: `XBTUNE_SHOT_WIZARD=N`, `XBTUNE_SHOT_REPORT=1|crash`,
  `XBTUNE_CRASH_TEST=panic|thread|segv|abort`, `XBTUNE_REPORT_URL=...`.
- Snap: `network` plug on the GUI app.
- 2026-10-15 owner feedback round: wizard plug status showed "?" because
  Inter has no ✔/✘. ✓/✗ still showed "?" on the owner's laptop, so the
  status is now a drawn dot (green/red/grey) + a word, no symbol at all
  (`XBTUNE_SHOT_PLUGS=1` fakes states for UI checks). `theme::tests` checks
  every non-ASCII UI character against the font's cmap. Side bar shows
  `$SNAP_VERSION` (git hash) so the owner can tell which build runs. Owner's BT MAC removed from hint
  text/docs/tests (hint is "XX:XX:XX:XX:XX:XX"). New views: Resets (TPS +
  AFV, live TPD/TPS V/AFV) and Settings (safety locks -> `diag::set_unlocks`
  + `Command::SetPermits` to the live session; startup; theme).
  Snap permissions still need `snap connect` (a snap can't grant its own);
  store auto-connect for bluez/raw-usb would remove that once published.

### Phase 6 contents

- `xbtune-core::burn`: `changes` (per-variable, human units, edited vs
  ECM-owned, blocked), `build_target` (ECM bytes + chosen vars), `plan`
  (differing 16-byte chunks; page 0 only LFuel1/KBaro), `check_link` +
  `LinkLimits`, `engine_stopped`, `still_matches`, `execute` (writes then
  read-back, mismatch re-read 3x), `afv_reset`, `check_identity`.
- Session: `PrepareBurn` (permit, engine, link, verified full read ->
  `<data>/backups/<ID>_<time>_before-burn.xbt`) -> `BurnPrepared`;
  `Burn{target,note}` (needs fresh prepare; version/ID, engine, link
  re-checked; ECM unchanged since backup; write + verify; history in
  `<data>/history/<ID>_<time>/{before,after}.xbt, burn.txt`) -> `BurnDone`.
- CLI: `burn TUNE --vars a,b|--all [--note] [--yes]` (lists and stops
  without a choice), `restore BACKUP`, `afv-reset`; typed confirmations
  "burn"/"restore"/"reset".
- App: Tune "Burn to ECM…", Diagnostics "AFV reset…", burn dialog
  (`burn_view.rs`): prepare -> review (edits pre-ticked, note, confirm)
  -> progress -> done / failed (exact pages; Retry, Restore backup).
- Sim: `WriteFault::{Ignore, FlipBit}`, `cut_power_after_writes`, `poke`.
- Tests: 12 burn + 2 session + CLI; XBTUNE_HOME keeps test files out of
  the user's folders (CLI tests set it).
- Hooks: `XBTUNE_SHOT_BURN=NAME=VALUE [XBTUNE_SHOT_BURN_GO=1]`.

### Phase 5 contents

- `xbtune-core::tune`: `Tune` (image + definition), `set` (encode, clamp,
  byte patches), undo/redo, `copy_from`, `modified_vars`, `save` (stamps
  `meta.modified`, `meta.parent_crc32`), `apply_op` (Set/Add/Multiply/
  Percent/Smooth/InterpolateRows/Cols), `Clip` (TSV), `partner`.
- Session `Command::ReadEeprom`/`CancelRead` -> `ReadProgress`/`EepromRead`/
  `ReadFailed`; verified, resumes after reconnect; `read_timeout` 1 s.
- App Tune view (`tune_view.rs`, `grid.rs`): open/save/save-as via an
  in-app file browser (no file-dialog crate), Read from ECM (auto-saved
  backup in images dir, never overwritten), variable list (edited = accent,
  differs from compare = ≠), map grid (heatmap, axes, drag/shift/ctrl
  select, row/col headers), ops toolbar, Copy/Paste (system clipboard TSV),
  link front/rear, 3D surface (mesh; drag to rotate), compare (other /
  difference), scalars (step = scale), flag bytes as checkboxes, Ctrl+Z/Y/S/
  C/V, arrows; unsaved edits -> recovery file on exit; last tune reopens.
- CLI: `set FILE NAME=V NAME[r,c]=V -o OUT`, `copy-table FROM INTO VARS -o
  OUT` (both refuse to overwrite the input without --in-place).
- Hook: `XBTUNE_TUNE=path XBTUNE_SHOT_TUNE="var=..;select=r,c:r,c;op=pct:5;3d;compare=PATH;mode=diff"`,
  `XBTUNE_SHOT_READ=1` (Read from ECM in the demo). ALWAYS set
  `XBTUNE_HOME=<scratch dir>` for test runs: it redirects config/data
  (backups, logs, settings) away from the user's real folders. (2026-10-14:
  a run without it left files in ~/.local/share/xbtune; trashed.)

### Phase 4 contents

- `xbtune-core::diag`: trouble codes (`codes_from_runtime`,
  `stored_codes_from_image`), `TESTS`, `safety_notice`, `Permit`,
  `run_function` (engine-stopped + idle checks, send once, busy polling,
  lost reply resolved by the busy state).
- Session `Command::Function` -> `FunctionProgress`/`FunctionDone` events;
  `SessionConfig.permit`.
- CLI: `codes [--clear]`, `test <name>|--list`, `tps-reset`; typed "yes"
  unless `--yes`; permit checked before connecting.
- App: Diagnostics view (codes, clear, tests, TPS reset, AFV note), confirm
  modal with safety checkbox, header "Code NN" alert, demo engine toggle.
- Sim (ASSUMED): busy for `test_duration` after a test, clear zeroes
  `HDiag*_LD`, `set_engine_running`, `Fault::LoseRequest`.
- Hardware unlock: `<Dirs.config>/hardware.toml` with `ecm_commands = true`
  (snap: `~/snap/xbtune/common/config/hardware.toml`). Set by the OWNER
  only (app Settings since 2026-10-15, or by hand). Never create it ourselves.
- AFV reset is NOT in phase 4: AFV is page-0 `LFuel1`, so it's an EEPROM
  write -> phase 6 burn path.

### Phase 3 contents

- `xbtune-core::session`: one tokio task owns the link (version, then
  runtime frames back to back); reconnect with backoff (1/8 of
  `reconnect_delay`, doubling); stops on unknown ECM, a different ECM after
  reconnect, or a runtime frame length that doesn't match the definition.
  CSV logging runs inside the task. `target_opener` + `open_error_text`
  shared by CLI and app.
- `xbtune-core::live`: channels (frame order), units, CSV logger
  (`docs/FILE_FORMATS.md`), rate meter.
- `xbtune-sim::engine`: made-up 30 s ride so live views move
  (`Simulator::bueib_running()`); CLI/app demo add 60 ms latency.
- CLI: `live` (`--channels/--all/--list/--count`), `log` (`--duration`,
  `--note`, Ctrl+C).
- `xbtune-app` (binary `xbtune`, snap app `xbtune` with gnome extension):
  Connect (BT scan/paired, manual address, USB ports, demo, snap-plug
  banner, last-used + connect-on-start), Dashboard (gauges, right-click to
  change; named I/O bits; current DTCs), Live data table, Charts (stacked,
  linked time axis), Record button, theme toggle. Settings in
  `Dirs.config/settings.toml`.
- UI check hook: `XBTUNE_SCREENSHOT=out.ppm XBTUNE_VIEW=dashboard|data|charts|connect|diag
  XBTUNE_SHOT_RUN=fan XBTUNE_SHOT_CONFIRM=front-injector
  XBTUNE_THEME=light XBTUNE_SHOT_AFTER=6 xbtune` (demo, no settings saved).

### Phase 2 contents

- `data/ecmdroid.toml`: full definitions for in-scope categories (4,679 EEPROM
  variables, 1,244 runtime, 92 bit sets, 831 names).
- `xbtune-defs`: model + decoding; checks on load; issues per ID
  (`xbtune-cli defs <ID>`, table in `docs/SUPPORTED_ECMS.md`).
- `xbtune-core`: `Image`, `.xbt` (`docs/FILE_FORMATS.md`), `.xpr`, `EepromRead`
  (resume + verify-by-double-read).
- CLI: `dump`, `show`, `diff`, `convert`, `defs`.
- Golden tests: EcmDroid's own test assertions (`crates/xbtune-defs/tests/`);
  owner's bike (`crates/xbtune-core/tests/owner_golden.rs`) skipped until
  `tests/fixtures/owner/` holds `xbtune.xbt`, `ecmdroid.xpr` and
  `ecmdroid_values.toml`.
- `docs/PARITY.md`: checklist from the public ECMSpy guides, with **NEW**
  items that weren't in KICKOFF §5.

## Layout

```
crates/xbtune-proto  PDU framing, checksum, decoder, EEPROM page model, transaction rules (no I/O)
crates/xbtune-link   serial + RFCOMM transports (BoxIo), EcmClient (retries/stats), BlueZ scan
crates/xbtune-defs   catalog + definitions (EEPROM/runtime vars, bit sets) from data/ecmdroid.toml; decoding (DDFI, DDFI-2, DDFI-3)
crates/xbtune-core   env (snap, paths), image (.xbt/.xpr), read (full EEPROM read), live, session, time
crates/xbtune-sim    simulated ECM + fault injection + made-up engine; in-process and pty; bin xbtune-sim
crates/xbtune-cli    xbtune-cli: info, ports, scan, ecms, doctor, dump, show, diff, convert, defs, live, log
crates/xbtune-app    egui app, binary xbtune (connect, dashboard, live data, charts); assets/fonts (Inter)
crates/xbtune-remote remote tuning: code, SPAKE2/HKDF/ChaCha20 crypto, relay client, local relay, session
relay/bug-report/    Cloudflare Worker: app reports -> GitHub issues (not in the snap)
relay/remote/        Cloudflare Worker + Durable Object: remote-tuning relay (not in the snap)
data/                ecmdroid.toml (ED-DB derived, RESTRICTED), ecm_models.toml (ours)
tools/               extract_ecmdroid_db.py
tests/fixtures/      EcmDroid test images and runtime frame
snap/snapcraft.yaml  core24, rust plugin, apps: xbtune (gnome ext), cli, sim; snap/gui desktop+icon
```

## Commands

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check
cargo run -p xbtune-cli -- info --sim
cargo run -p xbtune-sim -- --drop 0.1 --fragment 0.3   # prints /dev/pts/N
cargo run -p xbtune-cli -- info --serial /dev/pts/N
tools/test_remote_relay.sh              # remote relay Worker (wrangler dev) vs the Rust tests
xbtune-cli remote relay & xbtune-cli remote host --relay ws://127.0.0.1:8790/v1   # local remote session
snapcraft clean xbtune && snapcraft pack   # clean first, or the version label stays stale
sudo snap install --dangerous ./xbtune_0.1.0+git<HEAD>_amd64.snap   # name the file; never a glob
XBTUNE_SCREENSHOT=~/snap/xbtune/common/s.ppm XBTUNE_VIEW=charts xbtune   # GUI check inside the snap
```

Regenerating the data file needs EcmDroid's db (it is not in the repo):
`python3 -I tools/extract_ecmdroid_db.py ecmdroid.db <commit> > data/ecmdroid.toml`

## Decisions

- Ported from EcmDroid commit `2f17d29f`. Every ported file has a header
  citing the Java file and commit. Every fact is listed in `docs/PROVENANCE.md`.
- All ecmdroid.db-derived data lives in `data/ecmdroid.toml` and nowhere else.
  It is **restricted** until the owner confirms the GPL permission covers us.
- Decoder resyncs on SOH. It ignores echoes and other-address frames.
- Retries: reads up to 3 attempts, then a 150 ms settle drain. **Actuator
  commands are never retried** (`Request::is_idempotent`).
- `Layout::write_plan` refuses page 0. Page 0 writes need variable
  definitions (EcmDroid writes only `LFuel1`, `KBaro`). That is phase 6.
- RFCOMM connect (`xbtune_link::bluetooth::connect`) works around two
  problems in bluer 0.17's `Stream::connect`: it skips `bind()`, which the snap
  seccomp filter refuses with EPERM, and it waits with `poll()` for the real
  connection, because bluer returns early on a stale tokio writable event and
  the first write then fails with ENOTCONN.
- Bluetooth uses a direct RFCOMM socket (default channel 1, `--channel`).
  Discovery and pairing state come over D-Bus (bluer). No SDP lookup yet;
  if BuellTooth isn't on channel 1, add SDP via a BlueZ Profile.
- Storage (`xbtune_core::env::Dirs`), changed 2026-10-15 at the owner's
  request: the user's files go in a visible folder, `~/XBTune` (real home,
  `$SNAP_REAL_HOME`) by default or chosen in Settings (`<config>/folders.toml`,
  CLI reads it too), with `Tunes/ Backups/ Logs/ History/`. XBTune's own
  files (settings, hardware.toml, app log, pending reports) stay in
  `$SNAP_USER_COMMON/{config,data}`. This amends KICKOFF §3's "data under
  $SNAP_USER_COMMON": ~/XBTune also survives refreshes and reverts (and
  removal). Old `<data>/{images,backups,logs,history}` move over once at
  start (app and CLI; never overwrite; `env::move_old_layout`). Changing
  the folder can move the files too and sends `Command::SetStore`.
  `XBTUNE_HOME` -> `$XBTUNE_HOME/files`. Hook: `XBTUNE_SHOT_FOLDER=<dir>`.
- Snap apps: `xbtune` (GUI, gnome extension), `xbtune.cli`, `xbtune.sim`.
- Snap build does NOT use the rust plugin: the gnome extension's
  LD_LIBRARY_PATH (SDK libc) breaks host rustup/cargo, and snapd 2.76 in the
  build instance won't run the rustup snap as root. Parts are `nil` with
  scripts that unset LD_LIBRARY_PATH/PKG_CONFIG_PATH; Ubuntu's rustup deb
  installs stable (`rust-deps`). Verified in the snap 2026-10-14: GUI demo
  renders (fonts, GL), CLI live/log, BT absent -> "Host is down".
  Harmless denials: a11y bus (accesskit/zbus), /dev/char/195:0 (NVIDIA),
  cgroup cpu.max (Rust std thread count).
- GUI deps (listed in KICKOFF §4): eframe/egui 0.36 (default features off:
  glow, wayland, x11, accesskit), egui_plot 0.37. egui's bundled fonts are
  off (UFL/OFL in a crate fail cargo-deny); Inter 4.1 is bundled instead.
  `deny.toml` checks Linux targets only.
- Flags0-6 bit meanings are NOT in ecmdroid.db (all names empty), so idle /
  closed loop / WOT flags are shown raw. Need a real source before naming
  them (KICKOFF §5 lists them).
- Live polling uses `session::LIVE_TIMEOUT` (400 ms); EEPROM reads keep
  1 s. Owner logs: lost replies don't arrive late (intervals are ~0.15 s or
  ~1.3/2.5 s, almost nothing between), so a short timeout loses nothing.
- Sim demo (`live`/`log --sim`, app Demo) models BuellTooth: 35 ms latency +
  9600-baud wire time (`SimConfig::wire_baud`), ~143 ms per runtime poll.
- Gauge ranges are XBTune display presets (`xbtune-app/src/gauge.rs`); the
  db's runtime low/high are raw-type limits (Bat 0-2000 V).
- Publishing workflows are gated by the repo variable `XBTUNE_STORE_PUBLISH`.
- Tables/maps: layout is XBTune's reading of the db columns (EcmDroid only
  shows raw bytes). Maps = rows of `cols` values + `skip` pad bytes; `Table` =
  interleaved (input, output) pairs, input scaled by axisscale/axistranslate.
  Pair-table input scaling is UNCONFIRMED; `show` says so.
- Not ported: EcmDroid `Variable.getHigh()` double scaling. Duplicate db rows
  that disagree are dropped, not guessed (BUEOD `KVS_RPM_Revs`).
- `dump` verifies by default: a chunk counts only when two consecutive reads
  agree (the XOR frame checksum misses some errors). Backups must use it.
- Approved extra deps (2026-10-14): clap, thiserror, anyhow, tracing,
  tracing-subscriber, libc, futures; (2026-10-15) ureq, serde_json for bug
  reports. rustls is direct only to pick ureq's crypto provider: ureq's
  default TLS pulls webpki-roots (CDLA-Permissive-2.0, not on the license
  list), so it's built with `platform-verifier` (system CA certs). Ask
  before adding any others.

## Phase plan (amended by the owner 2026-10-15)

KICKOFF §8 lists 8 phases. The owner added remote tuning as phase 8, so
"Polish + release" becomes phase 9:
7. Log viewer + auto-tune (built 2026-10-15; owner testing).
8. **Remote tuning** (owner's idea, 2026-10-15; owner said go
   2026-10-15). `docs/REMOTE.md` APPROVED 2026-10-15 (code `47-381926`
   = room + 6 digits; review-each default; tuner may save a copy; separate
   `relay/remote/` Worker; x25519-dalek dropped). Owner AMENDED the "guest
   can't write" rule: per-session host permissions (edit directly / burn /
   tests+resets), all off at start; tuner-started burns run in the host's
   session with §6 checks, tuner confirms the review, host gets a 10 s
   countdown with Stop (REMOTE.md 2.1). Original decisions:
   - Bike owner = host (has the bike); tuner = guest. Host shows a 6-digit
     code, reads it to the tuner, who types it in: PAKE (SPAKE2), then an
     end-to-end encrypted channel (X25519/HKDF/ChaCha20-Poly1305). The relay
     only forwards ciphertext and rate-limits code guesses.
   - Relay: Cloudflare Worker (Durable Object + WebSockets) on the owner's
     account, like relay/bug-report.
   - Guest may view live data/dashboard, read shared logs, open the tune
     and propose edits, and REQUEST burns/tests/resets. Only the host
     connects to the bike, unlocks, accepts edits, and approves each
     burn/test/reset with the normal local review (KICKOFF §6 unchanged).
   - Edits: shared working copy, numbered ops (Tune patch undo model),
     journaled to the host's disk as they arrive; on a drop keep up to the
     last confirmed op, resync on reconnect; host files change only on the
     host's Save (temp + rename). Never auto-burn.
   - First-use guide dialog with "Don't show again".
   - Stages R1 relay+pairing+encrypted chat, R2 live view, R3 shared
     editing + journal, R4 approved requests, R5 log sharing. Write
     docs/REMOTE.md (protocol, threat model, tests) for owner approval
     before code.
   - Deps approved for it: tokio-tungstenite, spake2, x25519-dalek,
     chacha20poly1305, hkdf, sha2 (licenses must still pass cargo-deny).
9. Polish + release. Partly done (themes, Getting started guide with snap
   checks, CI promotion workflows). Left: store listing + auto-connect
   request (docs/STORE.md), docs, and the ecmdroid.toml licence question
   (blocks distribution, and remote tuning needs other people to install).
   Added by the owner 2026-10-15:
   - **Native title bar on GNOME Wayland**: eframe is built without
     winit's `wayland-csd-adwaita`, so GNOME shows the bare fallback
     frame. Turn it on (sctk-adwaita 0.12, MIT; check cargo-deny). It
     reads button-layout (left/right) and color-scheme from the settings
     portal by running `dbus-send`, and the title font via `gsettings`:
     make sure both exist in the snap (stage them if not) and test with
     buttons on the left. KDE/X11 draw their own frames already.
   - **Tube-frame DDFI ECMs**: DONE early (2026-10-15, see Status).
     Original note: (BUEGC, BUEIA, BUEJA, BUEKA; 1999-2002),
     which KICKOFF §1 had out of scope: EcmDroid supports them. Needs
     EcmDroid's ecmdroid.db again (not in the repo) to re-run
     tools/extract_ecmdroid_db.py with DDFI in IN_SCOPE; check EcmDroid's
     Java for every DDFI difference (protocol, pages, runtime frame) before
     assuming DDFI-2 behaviour; simulator + image tests; writes only after
     sim tests (§6.7) and stay unverified on hardware until someone has one.
   - No artwork/logo (owner's call; the Pegasus logo is a Buell trademark).
     Race ECMs: supported since 2026-10-15 (owner's request; see Status).

## Next steps

Paused 2026-10-14 at the owner's request (HEAD 8a55efa, tree clean; laptop
snap: xbtune_0.1.0+git8a55efa_amd64.snap). Resume here.


1. Owner reviews the phase 6 sim results (demo burn in the app), then
   decides whether to unlock writes and do the first real burn (checklist
   below). Suggested first real burn: a harmless, reversible change.
2. Optional: btsnoop capture of EcmDroid reading the EEPROM / live data, to
   confirm frames byte for byte.
3. Phase 7 built 2026-10-15: owner tests it (Phase 7 checklist). Then
   phase 8 remote tuning (see "Phase plan"; write docs/REMOTE.md first),
   only on the owner's go; then phase 9 polish + release.

## Phase 7 checklist (no ECM writes)

1. Warm the engine fully, then Record a log of steady riding: hold
   different steady throttle/RPM points for 10+ s each (closed-loop cells
   are cruise/light load). Or idle in the garage for a first look.
2. Logs -> Open log... -> the CSV: check duration/Hz, O2 rich/lean %, any
   anomalies; click a chart to move the playhead; Play, then Dashboard /
   Charts / Tune (live point moves over the fuel map).
3. Tune -> Read from ECM (or open a fresh backup) -> Fuel Map Front ->
   Logs: "Samples per cell", then "Average EGO" (blue < 100: ECM removed
   fuel; red > 100: added).
4. Auto-tune... -> Analyse -> review cells (untick any), Apply -> Undo
   works; Save as a new file. Nothing is written to the ECM.
5. CLI: `xbtune.cli logstats <log>`; `xbtune.cli autotune --tune <xbt>
   <log> -o ~/tuned.xbt`; `xbtune.cli diff <xbt> ~/tuned.xbt` (fuel cells only).
6. Optional, separately and only if happy: burn the corrected cells
   (normal Burn review), then Resets -> AFV reset, ride, log again: EGO in
   those cells should sit nearer 100%.

## Phase 6 checklist

Sim first (no unlock needed): app -> Connect -> Start demo -> Diagnostics ->
untick Demo engine "running" -> Tune -> open a tune, change a value ->
"Burn to ECM…" -> review -> tick -> Write. Also try AFV reset, and the
engine-running refusal. CLI: `xbtune.cli burn --sim <tune> --vars X --yes`.
Real bike, only after the owner signs off and unlocks writing (app ->
Settings -> "Allow writing to the ECM"):
1. Engine OFF, ignition ON, laptop close to the bike (or USB cable).
2. `xbtune.cli dump --bt ... -o ~/before.xbt` (extra copy).
3. First burn: one small reversible value, e.g. KRPM_Hard_Hi 7450 (stock
   7500): `xbtune.cli set ~/before.xbt KRPM_Hard_Hi=7450 -o ~/t1.xbt`,
   `xbtune.cli burn --bt ... ~/t1.xbt --vars KRPM_Hard_Hi --note "first burn"`.
4. Ignition off/on; `xbtune.cli show` a fresh dump: 7450. EcmDroid shows it too.
5. Put it back: `xbtune.cli restore --bt ... <backup path printed in step 3>`.

## Phase 5 checklist (no ECM writes; editing files only)

1. App -> Connect to the bike -> Tune -> "Read from ECM": progress bar,
   then it opens and is saved as a backup in the images folder.
2. Fuel Map Front: select cells (drag, shift-click, click row/column
   headers), try +, %, Smooth, Interp; Undo/Redo; 3D view; engine idling
   -> the "live point" circle should sit near idle RPM / low load.
3. Change a value (e.g. a rev limit) and a flag; Save -> asks for a new
   name (the ECM read is protected). Compare with the original -> ≠ marks
   and Difference view.
4. CLI: `xbtune.cli set ~/xbtune.xbt KRPM_Hard_Hi=7450 -o ~/test.xbt`,
   then `xbtune.cli diff ~/xbtune.xbt ~/test.xbt`.
5. Nothing here writes to the ECM; burning is phase 6.

## Phase 4 hardware checklist

Read-only part (no unlock needed), engine OFF, ignition ON:
1. `xbtune.cli codes --bt 00:11:22:33:44:55`; compare with EcmDroid's
   Trouble Codes screen.
2. App -> Diagnostics: same codes; a current code shows red in the header.
3. `xbtune.cli test fan --bt ...` must say "locked on real hardware".
Then, only if the owner signs off on the sim results and unlocks:
4. Engine OFF: `xbtune.cli test tachometer --bt ...`, then `test fan`.
5. Optional: `xbtune.cli codes --clear --bt ...` (stored codes only).
6. TPS reset only when the owner actually wants one (it changes the stored
   TPS zero; follow the printed procedure).

## Phase 3 hardware checklist (read-only)

Bike on its stand, somewhere ventilated. Snap: build here, install on the
laptop with `sudo snap install --dangerous xbtune_*.snap`; check
`snap connections xbtune` (bluez, raw-usb, removable-media connected).
1. Ignition ON, engine OFF: `xbtune.cli live --bt 00:11:22:33:44:55`.
   Expect RPM 0, sensible engine/air temp, battery ~12-13 V, ~6-7 Hz
   (the K-line limit), few errors. Compare a few values with EcmDroid's live screen.
2. Start the engine: RPM ~1000-1100 at idle, engine temp rising, O2 moving
   once warm. `xbtune.cli log --bt ... --duration 60 -o ~/idle.csv`.
3. GUI: open XBTune from the app menu. Connect -> Paired devices ->
   BuellTooth -> Connect. Check Dashboard, Live data, Charts; Record ~30 s.
4. Dropout: engine off, switch the ignition off for ~10 s and back on. The
   app should show "reconnecting" and come back by itself.
5. Close and reopen the app: "Last used" offers the BuellTooth.
6. Send: the CSV logs, and screenshots of anything odd.

## Phase 2 hardware checklist (read-only)

Ignition ON, engine OFF, kill switch RUN.
1. `xbtune.cli dump --bt 00:11:22:33:44:55 --xpr ~/BUEIB_xbtune.xpr -o ~/xbtune.xbt`
   (expect about 10-20 s, "Verified: yes", 0 re-reads).
2. `xbtune.cli show ~/xbtune.xbt`, then `... show ~/xbtune.xbt Tab_Fuel_Front Tab_Spark_Front`.
3. EcmDroid: fetch the EEPROM, save it, copy the `.xpr` to the laptop.
4. `xbtune.cli diff ~/xbtune.xbt ~/<EcmDroid file>.xpr` -> "Identical".
5. Send the three files plus a few EcmDroid Setup values (rev limits, fan temps).

## Phase 1 hardware checklist (read-only)

Ignition ON, engine OFF, kill switch RUN, adapter on the diagnostic connector.
1. `snap connections xbtune`, then connect bluez, raw-usb, removable-media.
2. `xbtune.cli doctor`: all plugs connected, data dir under ~/snap/xbtune/common.
3. `xbtune.cli scan`: BuellTooth listed. Pair/trust it with bluetoothctl if needed.
4. `xbtune.cli info --bt <ADDR>`: expect `Version: BUEIB…`, `ECM ID: BUEIB (DDFI-2)`,
   `Runtime: 107-byte frame (matches definition)`, `State: idle`, few retries.
5. `xbtune.cli info --serial /dev/ttyUSB0` with a USB cable: same result.
6. Compare the version string with EcmDroid's home screen.
