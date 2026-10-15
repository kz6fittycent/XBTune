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
  verified) differ in exactly 1 byte (0x01d2). A second burn followed
  (probably back to 7500): not yet confirmed; asked for the history/
  folders and app.log. Top bar showed 11 reconnects that session, 0% errors:
  check when they happened. AFV reset not yet tried on the bike.
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
- **Getting started + bug reports: built 2026-10-15** (owner asked; network
  feature OK'd by the owner per KICKOFF §9). Not yet on the laptop. The relay
  (`relay/bug-report/`) is NOT deployed yet: the owner does the Cloudflare/
  token steps in its README. Until then sends fail and reports queue in
  `<data>/app-log/pending/`.
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
crates/xbtune-defs   catalog + definitions (EEPROM/runtime vars, bit sets) from data/ecmdroid.toml; decoding
crates/xbtune-core   env (snap, paths), image (.xbt/.xpr), read (full EEPROM read), live, session, time
crates/xbtune-sim    simulated ECM + fault injection + made-up engine; in-process and pty; bin xbtune-sim
crates/xbtune-cli    xbtune-cli: info, ports, scan, ecms, doctor, dump, show, diff, convert, defs, live, log
crates/xbtune-app    egui app, binary xbtune (connect, dashboard, live data, charts); assets/fonts (Inter)
relay/bug-report/    Cloudflare Worker: app reports -> GitHub issues (not in the snap)
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

## Next steps

Paused 2026-10-14 at the owner's request (HEAD 8a55efa, tree clean; laptop
snap: xbtune_0.1.0+git8a55efa_amd64.snap). Resume here.


1. Owner reviews the phase 6 sim results (demo burn in the app), then
   decides whether to unlock writes and do the first real burn (checklist
   below). Suggested first real burn: a harmless, reversible change.
2. Optional: btsnoop capture of EcmDroid reading the EEPROM / live data, to
   confirm frames byte for byte.
3. Phase 7 (only after the owner says go): log viewer + auto-tune.

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
