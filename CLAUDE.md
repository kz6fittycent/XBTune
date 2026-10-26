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
- **Phase 9 wideband + disclaimer + store docs: built 2026-10-19**; see
  the phase plan and "Next steps".
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
- **Remote R2-R5: built 2026-10-15; owner test PASSED 2026-10-15** (snap
  62bd467, laptop = owner on the bike, desktop = tuner): "worked
  perfectly ... zero issues"; a remote AFV reset on the real ECM took.
  Owner likes the granular per-session permissions. PHASE 8 DONE. App logic `remote_sync.rs`, UI
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
  garage). ALL builds run on GitHub (owner, 2026-10-26: no local
  `snapcraft pack`, keeps root from filling). Take the snap from the
  "🧪 Snap Builds" run for the pushed commit: artifact named after the
  `.snap`, kept 14 days (`gh run download <run> -R kz6fittycent/XBTune`).
  The user installs it on the laptop with `sudo snap install --dangerous`. Interfaces must be
  connected on the laptop too. Can't run commands on the laptop: give the
  user exact commands and ask for the output.
- Phase 1 hardware result (laptop, snap git b5fa645): `info` OK, 23/23
  requests, 0 retries, RTT avg 65 ms (min 50, max 147).
- Snap confinement checks on this PC: see "Snap notes" below.
- Remote: GitHub repo `git@github.com:kz6fittycent/XBTune.git` (origin).
  PUBLIC since 2026-10-22, with a wiki (pages + screenshots in
  `XBTune.wiki.git`, images under `images/`; screenshots made with the
  hooks in demo mode, scratch paths painted over to `/home/rider/...`).
  Before going public the history was rewritten to replace the owner's
  BuellTooth MAC with `00:11:22:33:44:55`, so snap hashes noted below
  (23ea31c, a1bd3e7, ...) are pre-rewrite SHAs. NEVER put the owner's
  personal email anywhere public (snap `contact:` is the issues URL;
  repo-local git `user.email` is the GitHub noreply address).
  Store publishing ON since 2026-10-22 (docs/STORE.md): a push to main
  that passes Snap Builds AND carries a version not yet in the store goes
  to `latest/candidate` (call-for-testing.yml `gate` job); stable needs
  the owner's `/promote` + approval.
- Versions (owner, 2026-10-22): plain `0.x.y`, no `+git` suffix; releases
  count in threes, 0.1.0 -> 0.1.1 -> 0.1.2 -> 0.2.0. `tools/bump_version.sh`
  bumps (Cargo.toml + lock); bump once per release, in the commit pushed.
  Pushes without a bump are built and checked but not uploaded.

### Bike profiles + wideband logging mode (built 2026-10-25; sim/UI only)

Owner asked 2026-10-25: profiles per bike, everything reopened as left;
chose own folder per bike, "last one + match on connect", and the logging
mode helper for the LC-2 friend (who logs ECMSpy-style: O2/closed loop
off, AFV locked at 100).
- `env`: `Dirs.root` (chosen files folder) vs `Dirs.files` (=
  `root/Bikes/<active>` when `<config>/bike.toml` names an existing one,
  else root); `active_bike`/`set_active_bike` (one plain path component);
  `folder_pairs` includes Bikes; old-layout move goes to root. Session
  store, backups, history, logs, Remote, CLI all follow the active bike;
  `doctor` prints it.
- `settings::BikeState` (flattened into Settings, so old settings.toml
  reads unchanged): last target, gauges, charts, last_tune, wideband,
  wb_unit, view, tune_var, compare, overlay, logs, `AutotuneState`,
  `log_mode`. `profile_offer` (global) = "Don't ask again".
- `profiles.rs`: `Bikes/<folder>/bike.toml` = `[bike]` (name, model,
  year, notes, ecm_id, ecm_version) + `[state]`. `app.save_settings`
  captures the views into the state and writes the profile (settings.toml
  keeps the no-profile state in `app.general_bike`). Switch: saves, unsaved
  edits -> recovery file, stops the link unless it's the same bike,
  reopens tune/map/compare/logs/view. Blocked during remote/burn/read.
  Sidebar picker on top; New/Edit dialog (copy current settings; first
  profile can move root Tunes/Backups/Logs/History in, paths rebased).
  On connect (not demo): profile without ECM learns it; other ECM model ->
  banner (Use <match> / New / Keep); no profile -> offer once per run.
  Same ECM model can't be told apart (shared version string): no offer.
  No delete in the app (folder holds the user's files).
- `xbtune-core::logmode`: KFBFuel_Min/Max, KLFuel_Min/Max, LFuel/LFuel1
  -> 100; KCL_Fuel_Config bit 7 off where it exists (not on BUEIB).
  Saved originals in `BikeState.log_mode`; Restore puts limits/switch
  back, AFV stays 100. ASSUMED: EGO window 100..100 holds EGO at 100 on
  ECMs without the switch; first log with it should show EGO Corr. = 100.
  Setup view card. Burn is the normal path.
- Hooks: `XBTUNE_SHOT_BIKE=1|new|offer` (1 = load the active profile
  under XBTUNE_HOME in screenshot mode). Icons motorbike + pencil added.

### Phase 10 (Windows) contents

- Approved 2026-10-24: windows-sys 0.52 (serialport's version) for COM
  port names, embed-resource (build dep) for the .exe icon; BSL-1.0 as a
  deny.toml exception for clipboard-win + error-code only (egui clipboard).
- `xbtune-link`: own `BtAddress` (Target::Rfcomm/settings unchanged);
  `bluetooth` = common API + `bluetooth/bluez.rs` (Linux only; bluer and
  udev are Linux target deps). Elsewhere `bluetooth::AVAILABLE` is false
  and connect/scan return "pair it, pick its serial port". `list_ports`:
  Linux USB only; other OSes every port; Windows labels them via
  `win_ports` (SetupAPI instance ID: BTHENUM ...&<addr>_C... = Outgoing,
  000000000000 = Incoming; paired name from BTHPORT\...\Devices\<addr>
  \Name), outgoing BT first. macOS keeps only `/dev/cu.*`.
- `env`: files in `Documents\XBTune` on Windows, config/data in
  `%APPDATA%\xbtune`; non-Linux permission hint = "another program has
  the port open". AI config: 0600 on Unix only.
- Sim: pty mode Unix only (`xbtune-sim` says use `--sim`/demo).
- App: `windows_subsystem = "windows"`; window icon from
  `assets/icon-128.rgba`, exe icon `assets/xbtune.ico` (both made by
  `tools/render_icon.py` from snap/gui/xbtune.svg); Connect view: BlueZ
  card only where AVAILABLE, "Bluetooth or USB cable" card with pairing
  help elsewhere; wizard text; fatal-signal handler Unix only (panics
  still caught); report masking also hides `/Users/x` and `C:\Users\x`.
- CI: `windows` job in ci.yml (clippy, tests, release build, zip artifact
  `xbtune-windows-x64-<sha>`). Local type-check without mingw:
  `CC_x86_64_pc_windows_gnu=<stub that emits empty objects>
  AR_x86_64_pc_windows_gnu=llvm-ar-21 cargo clippy --target
  x86_64-pc-windows-gnu` (nothing links; CI is the real build).
- Update notice (`update.rs`, owner 2026-10-24: link to the release page,
  no winget): not in the snap; at start, once a day (settings
  `update_checked_at`), GET api.github.com/.../releases/latest (ureq via
  `report::agent`, stable only), newer than CARGO_PKG_VERSION and not
  `update_skip` -> banner above every view: Download (opens the release's
  html_url) / Skip this version / ×. Settings "Updates" card: switch,
  Check now, All releases link. Failures logged only. Hook
  `XBTUNE_SHOT_UPDATE=<version>`.
- Release workflow (2026-10-26): `windows-release.yml` (workflow_call
  from call-for-testing's gate as job `windows-release`, its own job so it
  never blocks the snap; also workflow_dispatch with `sha`): tests, builds,
  zips `xbtune-windows-x64.zip`, creates pre-release `v<version>` (re-run
  = replace the zip). promote-to-stable: after the snap release, the
  issue title's version -> `gh release edit --prerelease=false --latest`
  (security rebuilds skip). Issue body links the pre-release.
  The tag's commit must have main's workflow files, or GITHUB_TOKEN gets
  a 403 on create. v0.2.0 = e7604c5 (run 40349553273; 047da49 failed so).
- Rolling `windows-test` pre-release (owner, 2026-10-26: the friend tests
  every push): ci.yml job `windows-test-release` after `windows`, push to
  main only, tip-of-main check, deletes and recreates the release + tag
  on that commit, never latest. Link: releases/tag/windows-test.
  Local copy for the friend (no GitHub account yet): user timer
  `xbtune-windows-sync.timer` (every 10 min) runs
  `~/.local/bin/xbtune-windows-sync`, which puts that zip in
  `~/Nextcloud/iDraw/xbtune-windows-x64.zip` when the build changes.
- Not yet: Windows
  crash handler (SetUnhandledExceptionFilter), installer.

### Friend's files (2026-10-24; Windows tester with the LC-2)

He has no bike: he opened a tune from a wideband-tuned bike
(`~/XBTune/Logs/BUEIB340_20260524-1107 (1).xpr`, BUEIB, opens fine)
and an ECMSpy tune `~/XBTune/Tunes/dhtunerevision3.msq` (XML: veBins,
advTable, afrBins = per-cell target AFR, own axes; a DIFFERENT tune).
.msq import is NOT built: KICKOFF section 2 puts ECMSpy's files off
limits; OWNER TO DECIDE. His .msl log (20261017-1324.msl, "single CL
firmware", 100% O2 "sensor error") was not sent. Done for him:
sidebar scrolls on short windows (`XBTUNE_SHOT_SIZE=WxH` hook);
"Average AFR (wideband)" always shown, greyed with the reason
(`autotune_view::wideband_missing`); hint under fuel maps with no logs;
.msl: a column named AFR/lambda/wideband (not an ECM channel) is read as
the wideband (median decides lambda vs AFR, stoich 14.7), other columns
listed in `Log::unread_columns` (Logs view, amber); auto-tune names
missing required channels (`Analysis::missing_channels`: RPM, Load, EGO
Corr.); CLI `--maps` only takes front|rear|both. He is writing a list
of questions. Owner 2026-10-24: RPM low to high, and lambda as an option
(the friend tunes in lambda). BUILT: `GridSpec::flip_cols` (+ surface)
when the X axis is stored descending; selection/undo stay in storage
order, arrows and clipboard follow the screen (`grid::view_col`).
`settings.wb_unit` (Afr|Lambda; Connect wideband card, auto-tune dialog;
`app.set_wb_unit` swaps wb_afr/wb_lambda gauges and charts): header,
overlay label + cells, targets typed in the unit (`TargetAfr::parse`
takes AFR 9-17 or lambda 0.6-1.2, mixed refused; `lambda_text`),
proposals, lean list. Hook `XBTUNE_SHOT_WBUNIT=lambda`.

### UI refresh (owner asked 2026-10-24: "modern UX/UI"; he's not the judge)

BUILT 2026-10-24 (all 8 below). `icons.rs` (Lucide consts; `// lucide:`
names drive `tools/subset_lucide.py`, needs fontTools + `npm pack
lucide-static@1.54.0`), Lucide FIRST in every font family (Inter has its
own private-use glyphs; tests: subset maps only PUA, every const is in
it). `theme`: `primary` (ACCENT_FILL d95600, white text), `icon_button`,
`toggle` (switch), `nav_item`/`nav_group`, `dialog_buttons` (Windows:
main first; else last; right-aligned), `progress`; radius 6, checkbox
18 px, hairline borders, tinted selection, inactive bg_fill = page bg
(empty checkboxes/radios), menu_margin 14 (dialogs). `apply` also sends
ViewportCommand::SetTheme (Windows title bar). Settings `window`
(size + maximized; no position) restored in main.rs except in
screenshot mode. Sidebar: Connect / RIDE / TUNE / SERVICE, bottom:
Getting started, Report a problem, Settings, version + lock state.
Header: link stats in the status tooltip (error % only when > 0),
sun/moon theme icon. Tune: title row, file row (Burn last), cell-op
icon buttons, "?" popover with how-to + shortcuts.
Review of every view (demo screenshots, both themes) found, in order:
1. Windows title bar follows the app theme (else white over dark).
2. Remember window size/position/maximized.
3. Dialog buttons in the OS's order (Windows: primary first; GNOME: last).
4. Sidebar: Lucide icons, left-aligned, tinted selection + accent bar,
   grouped by task, Settings pinned at the bottom.
5. One accent-filled primary button per view; consistent danger style.
6. Toggle switches for on/off settings; checkboxes that look like them
   (an unticked one renders as a grey dot today).
7. Header: link stats into the status tooltip; theme = sun/moon icon.
8. Tune toolbar on one row (icon buttons + tooltips); "?" popover
   instead of the long "1. Select cells..." line.
Icons: Lucide (ISC) as a bundled font, like Inter (owner chose it
2026-10-24 over Fluent/Phosphor).

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
crates/xbtune-ai     AI advisor: provider presets, ai.toml, OpenAI-compatible + Anthropic clients, prompt, journal
relay/bug-report/    Cloudflare Worker: app reports -> GitHub issues (not in the snap)
relay/remote/        Cloudflare Worker + Durable Object: remote-tuning relay (not in the snap)
data/                ecmdroid.toml (ED-DB derived; cleared 2026-10-22), ecm_models.toml (ours)
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
lxc delete --project snapcraft snapcraft-xbtune-amd64-61210625 --force   # after a build: frees the root disk
sudo snap install --dangerous ./xbtune_0.1.0+git<HEAD>_amd64.snap   # name the file; never a glob
XBTUNE_SCREENSHOT=~/snap/xbtune/common/s.ppm XBTUNE_VIEW=charts xbtune   # GUI check inside the snap
```

Regenerating the data file needs EcmDroid's db (it is not in the repo):
`python3 -I tools/extract_ecmdroid_db.py ecmdroid.db <commit> > data/ecmdroid.toml`

## Decisions

- Ported from EcmDroid commit `2f17d29f`. Every ported file has a header
  citing the Java file and commit. Every fact is listed in `docs/PROVENANCE.md`.
- All ecmdroid.db-derived data lives in `data/ecmdroid.toml` and nowhere else.
  It was restricted until 2026-10-22, when the owner got approval to publish.
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
7. Log viewer + auto-tune (built 2026-10-15; owner tested: auto-tune works).
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
     2026-10-15: EcmDroid issue #24 statement recorded in PROVENANCE; the
     owner emailed the EcmDroid maintainer asking whether the ECMSpy
     permission covers downstream GPLv3 projects. Waiting for his reply;
     data stays restricted until then.
   Added by the owner 2026-10-15:
   - **Native title bar on GNOME Wayland**: BUILT 2026-10-16. Direct
     `winit` dep (0.30, default features off) with
     `wayland-csd-adwaita-crossfont` -> sctk-adwaita **0.10** (what winit
     0.30 uses; not 0.12). Not `wayland-csd-adwaita`: its ab_glyph (and
     sctk-adwaita's skrifa) embed Cantarell, OFL, not allowed in deps by
     KICKOFF §2. crossfont links system fontconfig + FreeType; build needs
     `libfreetype-dev libfontconfig-dev` (snap build-packages, CI), or
     freetype-sys silently compiles its bundled FreeType. At run time it
     calls `dbus-send` (core24; portal button-layout + color-scheme),
     `gsettings` and `fc-match` (gnome-46-2404). Its "unknown button type"
     warning (empty side of the layout) is filtered in `report.rs`.
     KDE/X11 draw their own frames already. Owner's desktop has buttons on
     the left (`close,minimize,maximize:`).
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
   Added by the owner 2026-10-16 (go for phase 9, leftovers included):
   - Wideband: BUILT 2026-10-19 (option B). The owner's friend uses
     Innovate LC-2s ("Very simple"). `xbtune-proto::isp2` (decoder +
     encoder, from Innovate's ISP2 spec PDF, PROVENANCE "Wideband"),
     `xbtune-core::wideband` (task, `WbLink::serial`, fresh <= 300 ms,
     pseudo-channels `wb_afr`/`wb_lambda` with offset 1<<30 that
     `Sample::value` reads from `Sample.wb`), `SessionConfig.wideband`,
     `Event::Wideband(WbState)`, CSV `# wideband:` + wb_lambda/wb_afr/
     wb_status columns, `Log.wideband`. Auto-tune `Mode::Wideband{target:
     TargetAfr, lag}`: correction = EGO x measured/target lambda, open loop
     and WOT count, `Analysis.lean` (AFR > 14 at load >= 80). CLI live/log
     `--wideband PORT|sim`, autotune `--wideband --target --lag`. App:
     Connect "Wideband O2" card (`settings.wideband`), header dot + AFR,
     gauge, AFR overlay, dialog mode/targets/delay; remote `FrameMsg.wb`.
     Sim LC-2: `xbtune-sim::wideband` (in-process; `xbtune-sim
     --wideband` pty). Hook `XBTUNE_SHOT_WIDEBAND=1`, overlay `afr`.
     Sim-tested only. Analog 0-5 V (option A) and AEM not built.
   - "Fold AFV into the maps, then AFV reset" helper (via the burn path).
     BUILT 2026-10-16: `autotune::{afv_fold_plan, fold_afv,
     has_autotune_step}` (one undo step; front uses LFuel1 only with
     KCL_Fuel_Config bit 0, PROVENANCE), CLI `fold-afv FILE -o OUT` (prints
     the burn --vars line), app Tune "Fold AFV into the maps…" on fuel maps
     (`afv_fold_view.rs`; blocked while the undo history has an auto-tune
     step: those corrections already include AFV). Hook
     `XBTUNE_SHOT_FOLD=1`. Owner's dump: AFV 101.8% -> 276 values change.
   - Binary / .msl log formats (EcmDroid `Bin2MslConverter` as reference).
     BUILT 2026-10-16: `xbtune-core::logformats` (EcmDroid .bin read, .msl
     read + write; `Log::open` sniffs the content; `Log.source`). .msl
     rebuilds frames from columns named like runtime channels; log keeps
     only the channels found. CLI `log-convert IN -o OUT.msl|.csv`;
     logstats/autotune take .bin/.msl. Logs view lists .bin/.msl, shows
     the source, "Export .msl" (Logs folder). Fixtures from ED tests in
     `tests/fixtures/ecmdroid/` (PROVENANCE). docs/FILE_FORMATS.md.
   - burn.txt lists the changed variables (old -> new) and "verified: yes".
     BUILT 2026-10-16: `burn::history_lines` + `ecm:` version line in
     `Worker::save_history` (only verified burns get a history folder).
11. **AI-assisted tuning** (was phase 10; owner swapped it with Windows/
   macOS 2026-10-24) (owner slated it 2026-10-16). Ground rules
   agreed when slating it: the AI is an advisor, never an actor. Its
   suggestions arrive as proposals through the same path as a remote
   tuner's (R3 review: Accept/Reject, one undo step), bounded by the
   deterministic auto-tune limits (cap, min hits, CLT filter); it never
   burns, runs tests or resets (KICKOFF §6 unchanged). Network feature ->
   needs the owner's OK per KICKOFF §9; nothing sent without a click,
   with a "What gets sent" preview like bug reports. Write docs/AI.md
   (provider/API key/cost, what data leaves the machine, prompts, how
   numbers are checked, tests) for approval before code. Wideband
   (phase 9) makes it far more useful.

   Added 2026-10-19: XBTune stays 100% FREE (owner): no paywall, no ads
   (ads advised against: GPL code means any lock is removable, ad SDKs
   on a Linux desktop app pay almost nothing and need tracking). Engine-
   damage warning BUILT: `xbtune-core::disclaimer` (versioned), app modal
   at first start until accepted (`settings.disclaimer_accepted`; hook
   `XBTUNE_SHOT_DISCLAIMER=1`), short line in burn/auto-tune/fold dialogs
   and CLI burn/autotune/fold-afv/--help. README, snap description and
   docs/STORE.md listing updated.
   Phase 10: docs/AI.md APPROVED 2026-10-19 (spark maps in, 0600 key, all
   providers, no default model until A3, hosted OK, bug-report helper
   = A4). A1 BUILT 2026-10-19: `crates/xbtune-ai` (`config` presets +
   ai.toml 0600 + XBTUNE_AI_KEY, `client` prepare/send/list/test for
   OpenAI-compatible and Anthropic raw HTTP (fallbacks on current Claude
   models), `context` SYSTEM_PROMPT v1 + data_block (map, partner map,
   AFV, per-cell stats with load/rpm and a words reading, auto-tune
   proposals, log summary; nothing personal), `journal` ~/XBTune/AI/
   *.jsonl). CLI `ai presets|setup|models|test|ask` (typed "send" unless
   --yes). App: Settings "AI advisor" card (`ai_view::settings_card`),
   Tune "Ask the AI advisor…" on fuel/spark maps (`ai_view.rs`: guide
   `settings.ai_guide_seen`, preview, thread, Cancel, warnings for cut
   input/output). Disclaimer v2 (AI point; everyone sees it again).
   Report masking hides the key. Hook `XBTUNE_SHOT_AI=guide|ask|preview|
   answer[:MAP]`. Tested with mock servers (both adapters) and local
   Ollama qwen3-coder (findings in AI.md). Next: A2 (structured
   proposals + checker + review), A3 (eval), A4 (bug-report helper).
   HIDDEN 2026-10-22 (owner): `xbtune_ai::enabled()`, see Next steps.
10. **Windows and macOS** (was phase 11; owner moved it ahead of AI and
   said GO for Windows 2026-10-24: the friend with the LC-2 wideband
   wants to test, so Windows first) (owner's plan, 2026-10-22; was: wait
   for the owner's go). **Linux is and stays the first-class platform**:
   features land and are tested there first, the snap is the reference
   build, and Windows/macOS must never hold back or complicate a Linux
   release. They get the same features where the OS allows, released
   from the same version.
   - Distribution: installers live on **GitHub Releases**. **No winget**
     (owner, 2026-10-24: "99% of Windows users don't even know what
     Winget is"): Windows users download from the release page and the
     app's update notice links there. macOS via a **Homebrew tap**
     (`kz6fittycent/homebrew-tap`). **No Chocolatey** (owner, 2026-10-22):
     we don't publish there; someone else may package it if they want.
     No paid code signing or notarization to start (Windows users get
     SmartScreen's "More info -> Run anyway" once; say so in the notes);
     on macOS start with a formula installing a prebuilt binary (a
     non-notarized cask hits Gatekeeper; re-check current Homebrew
     behaviour before building). Check the `xbtune` name is free on
     Homebrew first.
   - Releases: one version for every platform. The version the
     call-for-testing `gate` finds new also builds Windows (x64 zip;
     an installer later if needed) and macOS (arm64 + x64)
     in Actions and attaches them to a GitHub **pre-release** `v0.x.y`.
     `/promote` to stable makes it the latest full release and only then
     updates the tap. Windows/macOS jobs failing
     must not block the snap.
   - Updates (owner): the Windows/macOS builds check GitHub Releases and
     tell the user; they never download or install anything themselves.
     At most once a day, at start, `GET
     api.github.com/repos/kz6fittycent/XBTune/releases/latest` (stable
     only; unauthenticated, ureq); newer semver -> a dismissible banner
     with the version and a link that opens the GitHub release page
     (Windows: the release's page, where the zip is; macOS: `brew
     upgrade xbtune` if installed by Homebrew, else the release page);
     "Skip this version"; a Settings switch to turn the check off;
     failures are silent (logged). The snap does NOT check (snapd
     updates it); a non-snap Linux build may use the same notice.
     Network use OK'd by the owner (KICKOFF §9).
   - Code: Bluetooth on Windows/macOS = the OS's serial port for a
     paired SPP adapter (`COMn`, `/dev/cu.*`), so it uses the existing
     serial path; the Connect view gets a "Bluetooth serial port" picker
     there and hides the BlueZ scan. `#[cfg]`-gate the Linux-only parts:
     `xbtune-link/src/bluetooth.rs` (bluer, RFCOMM), `report.rs` signal
     handler, `wayland_dnd.rs`, snap plug checks and wizard text,
     serialport's `libudev` feature. `env::Dirs`: `Documents\XBTune` /
     `~/XBTune` via `directories`. `deny.toml` gains the new targets.
     CI: windows-latest and macos-latest build + test jobs.
   - Order: Windows first (most riders' garage laptops); macOS
     + the Homebrew tap **last** of the other OSes (owner). Each starts simulator-tested
     only; the first hardware check is a Windows laptop on the bike
     (read-only: info, dump, live, codes) before any write on it.
     macOS classic-Bluetooth serial has been flaky on recent releases:
     USB cable may be the supported Mac path.

## Next steps

Resume here (2026-10-26, end of session): PAUSED, waiting for the LC-2
friend's input (Windows tester; no GitHub account: he gets
`~/Nextcloud/iDraw/xbtune-windows-x64.zip`, synced from `windows-test`
by the user timer). State:
- Store `latest/candidate` = 0.2.3 rev 9 (3a4f3a5), call for testing #11;
  stable still empty until the owner's `/promote 9 stable done` (that also
  makes GitHub `v0.2.3` the latest full release: first real run of that
  promote step, check it).
- GitHub: pre-releases v0.2.0-v0.2.3 (Windows zip each), rolling
  `windows-test` (every push to main). Every push builds Windows now.
- Wiki current as of 0.2.3 (f005d33). Keep it current with every
  user-visible change.
- Versions: the owner asked for 0.2.3 after 0.2.2, but CLAUDE.md's rule
  (and `tools/bump_version.sh`) count in threes (0.2.2 -> 0.3.0); the
  script made 0.3.0 (be9ad7d, never uploaded) and 3a4f3a5 set 0.2.3 by
  hand. Owner 2026-10-26: next release is 0.3.0 (threes rule stands;
  0.2.3 was a one-off); `tools/bump_version.sh` gives 0.2.3 -> 0.3.0.
- Older waits still open: the friend's ECMSpy switches + a first log with
  logging mode on (EGO Corr. = 100?), his .msl, the .msq import decision,
  the owner's laptop test of bike profiles.

2026-10-26: bumped to 0.2.0 and pushed (owner's go) -> store candidate + call for testing.
Then 0.2.1 (owner): map cells take typed numbers (8e2481f); first automatic Windows pre-release.
0.2.1 went through by itself: candidate + pre-release v0.2.1 (cabd79d).
Wiki updated for 0.2.1 (555e65f): new pages Windows, Bike-Profiles; all
screenshots retaken in demo mode (script idea: one XBTUNE_HOME per shot,
XBTUNE_SHOT_BIKE=1 with a made-up "2005 XB9SX" profile; paths painted to
/home/rider/...; user namespaces are blocked here). KEEP THE WIKI CURRENT
with every user-visible change (owner, 2026-10-26).
0.2.2 (owner): in-cell editing (`grid::CellEdit`: typing on selected
cells opens an editor in the anchor cell, Enter sets all cells selected
then, Esc cancels, click-away set (changed in 0.2.3); double-click/F2 edits the old value;
not in overlay modes; egui-driven test in grid.rs needs theme::install
fonts or cursors don't place) and .xpr IDs (`Image::load_preferring`:
name > connected ECM / compared tune > street ID of a shared definition;
B2RIB has its own category, so it isn't interchangeable with BUEIB).
0.2.3 (owner): the cell editor edits `TuneUi.value_text` (shared with the
toolbar box), so = + − × % use what was typed; click-away no longer sets
(closes, keeps the number); Esc restores the box's old text.

Resume here (2026-10-25, end of session): PAUSED. Bike profiles + wideband
logging mode pushed (22264be, then 9383229 snap-from-git fix); CI green on
both (CI incl. the windows job, Snap Builds, Call for Testing: no version
bump, so nothing uploaded). Local snap `xbtune_0.1.2_amd64.snap` (9383229)
built for the laptop. Owner plans 2026-10-26: push to GH and let the
Windows + Linux builds finish; if a store candidate / Windows pre-release
is wanted, bump to 0.2.0 (`tools/bump_version.sh`) in that push. Waiting for: the owner's
laptop test of profiles (first profile with "move files", switch to No
profile and back, connect -> ECM learned) and the logging mode card; the
LC-2 friend's answer on which exact ECMSpy switches he flips (the BUEIB has
no closed-loop switch; we pin EGO min/max at 100 instead: ASSUMED) and a
first log with the mode on (EGO Corr. should read 100 throughout); plus
the older items below (friend's Windows report/questions/.msl, .msq
decision). Next release = 0.2.0.
Snap builds: the part now uses `source-type: git` (committed files only);
before that `source: .` copied the host's 63 GB target/ into the LXD
instance twice and filled / (2026-10-25 cleanup: old build containers,
disabled snap revisions, docker build cache removed).

Resume here (2026-10-22, end of day): WAITING FOR FEEDBACK. Nothing is
in progress; continue as reports come in from the owner (Phase 9
checklist below on the laptop), testers (issues; call for testing #6 =
0.1.1 revision 4), Michel (EcmDroid maintainer; the owner shared the repo
and asked for his input: note anything relevant in PROVENANCE) and the
owner's friend with the LC-2 wideband. State at the pause:
- Store: `latest/candidate` = 0.1.2 rev 5 (a8eb7d8, 2026-10-24: Windows
  port, update notice, UI refresh; call for testing #7); stable empty
  until the owner's `/promote`. Next release = 0.2.0 (was: 0.1.1 rev 4 from
  7174ce1; next release then = 0.1.2
  (`tools/bump_version.sh`). Revs 2-4 and issues #4-#6 came from the
  history rewrite: a rebuilt 0.1.0 commit replaced 0.1.1 (rev 3, #5) and
  the rerun put it back. The gate now uploads only a version newer than
  everything in the store, and only from the tip of main.
- History rewritten 2026-10-22 twice (BuellTooth MAC, then the owner's
  Gmail -> noreply in all 108 commits + the wiki). The owner ran the email
  one (`git filter-branch` + force-push are blocked for Claude by auto
  mode; give the owner a script to run with `!`). A GitHub Support purge
  of the old SHAs is optional (risk explained as low; owner undecided).
- 2026-10-24: owner swapped phases 10/11 and said GO for Windows (phase
  10 now). See "Phase 10 (Windows) contents". Icon fixed (orange arc was
  off the gauge circle). CI green on windows-latest (1037b39). Public
  pre-release `v0.1.1-windows-test.1` (zip from CI run 40126429676) for
  the friend with the LC-2, made 2026-10-24 (owner chose a pre-release).
  Second: `v0.1.2-windows-test.1` (7d58899, CI run 40139928972: lambda,
  RPM low-high, .msl wideband, hints, sidebar). WAITING for his report. Done since: the update notice (link to the release page; no
  winget), then the UI refresh (owner asked for modern UX; Lucide icons
  approved 2026-10-24: see "UI refresh").

0. 2026-10-22: splash screen (`splash.rs`: tach needle sweep, name
   fades in, ~2.4 s, click/key skips; Settings -> Startup "Show the
   splash screen" = `settings.show_splash`; hook `XBTUNE_SHOT_SPLASH=<s>`
   freezes it at that time). Owner previewed and approved it. Released
   as 0.1.1 with the new versioning.
0. 2026-10-22 (later): repo PUBLIC, wiki written, README rewritten
   (install, snap connections, channels, screenshots), snap
   `grade: stable` + store links, publishing switched on. Left: the
   auto-connect forum request (docs/STORE.md; owner's post), store page
   screenshots/categories by hand, and possibly rewriting the old
   commits' author email (owner to decide).
0. 2026-10-22: owner got approval to publish (ecmdroid.db licence
   cleared). AI features HIDDEN, not removed (owner: riders won't trust a
   model with tuning): `xbtune_ai::enabled()` (env `XBTUNE_AI=1`) gates
   the Settings card, Tune "Ask the AI advisor…", bug-report "Check with
   the AI first" and the disclaimer's AI point; CLI `ai` hidden from
   --help; README section removed; docs/AI.md says so. Next: store
   (docs/STORE.md "Before the first upload").

0. Owner tried snap dedb0f4 on this desktop 2026-10-19: "okay better"
   (AI dialog without the review step). Drag and drop WORKS on his GNOME
   Wayland desktop (confirmed 2026-10-19). Setup view and the AI guide not
   reported on separately yet.
0. 2026-10-19 later: ALL BUILT and committed locally (not pushed, no snap
   built; owner asked to slow down): A2, location/air, Setup screen +
   baro, AI setup guide, import anything + drag and drop (X11 via egui,
   Wayland via `wayland_dnd.rs` on winit's connection, stoppable at exit;
   CLI file args; desktop Exec %F, MimeType text/csv). Wayland drop
   confirmed by the owner 2026-10-19.
   Then A3 (eval) and A4 (bug-report check) BUILT too (AI.md 9a). Phase
   10 complete except measuring more models (hosted ones cost money:
   owner's call; re-run qwen3-coder with OLLAMA_CONTEXT_LENGTH=16384).
   Left: friend's wideband wizard (deferred), store (licence), owner's
   laptop/desktop test of everything since a64264c (needs a push or a
   local snap build: ask first).
1. Order agreed 2026-10-19: A2 (BUILT, see AI.md 9a) -> location
   (offline standard pressure from altitude + Open-Meteo lookup by town on
   a click; owner OK'd the network use) -> EcmDroid-style named Setup
   screen incl. airbox pressure/baro (port ecm_setup.xml, GPL) + Baro ADC
   on the dashboard ("no sensor" at 255; owner's bike reads 254-255) ->
   AI setup wizard like Desktop Scriptures (scan localhost ports incl. the
   inference snap on 8328, cards, pick model) -> import anything + drag
   and drop (Wayland via wayland-client + smithay-client-toolkit, deps
   APPROVED) + file associations -> A3 eval, A4 bug-report helper.
   Owner asked to slow down snap builds and pushes: commit locally, build
   only when asked.
2. Owner on the laptop: CI snap for c85405c or later has the disclaimer,
   wideband (Connect -> Wideband O2), GNOME title bar, burn.txt listing,
   AFV fold and .bin/.msl logs; none tried on the laptop yet. A real LC-2
   (the friend's) on a USB serial cable is the first hardware test for
   the wideband: `xbtune.cli live --bt ... --wideband /dev/ttyUSB0`, then
   log a ride and `autotune --wideband`.
3. RUSTSEC-2026-0370/0371/0372 ("unmaintained" zvariant, zbus_names,
   zvariant_derive, via eframe -> accesskit_unix and zbus) are ignored in
   deny.toml with reasons (2026-10-19). Drop the ignores when egui moves on.
4. Licence: CLEARED 2026-10-22 (owner got approval to publish, "no
   issues at all"; PROVENANCE). Making the repo public and the store
   upload are still the owner's call: ask before either.
5. Store: docs/STORE.md "Before the first upload".
6. Untracked `app.log` / `fatal.log` in the repo root are the owner's
   copies from the laptop; not committed.
7. FUTURE (owner, 2026-10-19; not yet): the wideband will be tested by the
   owner's friend in Ohio, remotely. Needs a temporary guided wizard for
   him (install, connect the LC-2, log, send the log back). Don't build
   until the owner says so.

## Phase 9 checklist (laptop; no ECM writes needed)

Covers everything built since the laptop's last snap (c85405c): the
store build is the easy way in.

0. `sudo snap refresh xbtune --channel=latest/candidate --amend` (from
   the local --dangerous install to the store's 0.1.1+), then
   `snap connections xbtune`: bluez, raw-usb, removable-media connected
   (reconnect any that aren't).
1. Start XBTune: the splash (tach needle sweeps up and back, name fades
   in, ~2.4 s); a click or key skips it. Side bar shows `v0.1.1`.
2. Then the "Tuning can damage your engine" warning (5 points, no AI
   point): Continue stays grey until the box is ticked; Quit closes.
   After Continue the Getting started guide opens. Restart: splash, no
   warning. Settings -> Startup -> untick "Show the splash screen" ->
   restart: no splash; tick it again.
3. No AI anywhere: Settings has no "AI advisor" card, Tune has no "Ask
   the AI advisor..." button, Report a problem has no "Check with the AI".
4. GNOME: native title bar with your buttons on the left.
5. Connect to the bike (ignition on, engine off). Setup (side bar) ->
   compare the switches and values with EcmDroid's Setup screen (rev
   limits, fan temps, the 26 values in tests/fixtures/owner/). The baro
   reading shows "no sensor" (your bike's Baro ADC reads 254-255).
6. Drag a .xbt backup and a .csv log from the Files app onto the window:
   the tune opens in Tune, the log in Logs. Also `xbtune <file>` from a
   terminal.
7. Connect -> Wideband O2 -> "Simulated LC-2", then connect to the bike:
   header shows a dot + "AFR 12.x" after ~6 s (warming first); dashboard
   has a Wideband AFR gauge. Record 1-2 min, then Logs -> open it ->
   Tune -> fuel map -> "Average AFR (wideband)" overlay -> Auto-tune...
   shows the Wideband mode (target AFR, sensor delay). Nothing is written.
   Set Wideband back to None afterwards (the simulated values are made up).
8. Logs -> Export .msl; `xbtune.cli log-convert <log> -o ~/x.msl`.
9. Tune -> "Fold AFV into the maps..." on a backup copy: preview, Apply,
   Undo. Don't burn it unless you want to.
10. Optional (writes, needs the unlock): burn something small and
   reversible (KRPM_Hard_Hi 7450, then back to 7500): the History
   folder's burn.txt lists the variables old -> new and "verified: yes".
11. Send back: anything odd (screenshots), and `~/snap/xbtune/common/data/app-log/app.log`
   if something failed.

## Remote tuning checklist (R2-R5; laptop = bike owner, desktop = tuner)

Both on the same snap. Laptop: connect to the BuellTooth, open a tune
(Tune -> Read from ECM or a backup), Remote -> Start session; desktop:
Remote -> Join.
1. R2: desktop Dashboard/Charts move with the bike; header "Remote ·
   <owner>'s bike". Laptop Disconnect -> desktop shows not connected.
2. R3: desktop Tune shows "(shared by ...)". Change a value -> laptop
   Remote shows the proposal (and a Tune banner); Accept -> desktop's
   value stays, laptop Undo puts it back on both. Reject -> desktop's
   value goes back. Laptop edits appear on the desktop. Tick "Edit the
   tune directly" -> desktop edits land at once. Desktop Wi-Fi off,
   edit, Wi-Fi on -> the edit arrives.
3. R4 (engine off, ignition on): desktop Diagnostics -> Fan -> Ask ->
   laptop prompt -> Review... -> normal confirm -> runs; desktop sees
   "done". Tick "Run tests and resets" -> desktop Fan -> Run -> laptop
   10 s countdown; press Stop once, let it run once. Burn: with writes
   unlocked on the laptop and "Burn" ticked, desktop changes
   KRPM_Hard_Hi, Tune -> Burn to ECM... -> Start -> desktop reviews ->
   Burn -> laptop countdown -> written and verified; burn.txt says who.
   Restore it from the laptop (Restore backup or burn 7500 back).
4. R5: laptop Logs -> open a log -> Send to tuner -> desktop Logs shows it.
5. End session on either side; desktop with "Save a copy" off: Save is
   greyed and the tune closes at the end.

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
