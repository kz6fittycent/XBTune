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
- **Phase 3 (live data + logging + first app): built 2026-10-14, waiting for
  the owner's hardware test.** Don't start phase 4 until the owner says so.
- Owner's bike: **2005 Buell XB9SX**, ECM **BUEIB** (DDFI-2), version string
  `BUEIB310 12-11-03`, BuellTooth `00:11:22:33:44:55` (RFCOMM channel 1).
- **Hardware tests run on a separate laptop** (the desktop can't reach the
  garage). Build the snap here, then hand the user the `.snap` file to install
  on the laptop with `sudo snap install --dangerous`. Interfaces must be
  connected on the laptop too. Can't run commands on the laptop: give the
  user exact commands and ask for the output.
- Phase 1 hardware result (laptop, snap git b5fa645): `info` OK, 23/23
  requests, 0 retries, RTT avg 65 ms (min 50, max 147).
- Snap confinement checks on this PC: see "Snap notes" below.
- Repo is private, with no remote. Don't add a remote or push without asking.
  Don't publish to the store.

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
- UI check hook: `XBTUNE_SCREENSHOT=out.ppm XBTUNE_VIEW=dashboard|data|charts|connect
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
snapcraft pack && sudo snap install --dangerous xbtune_*.snap   # LXD works again since the Docker fix
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
- Snap stores everything under `$SNAP_USER_COMMON` (`xbtune_core::env::Dirs`).
- Snap apps: `xbtune` (GUI, gnome extension), `xbtune.cli`, `xbtune.sim`.
- GUI deps (listed in KICKOFF §4): eframe/egui 0.36 (default features off:
  glow, wayland, x11, accesskit), egui_plot 0.37. egui's bundled fonts are
  off (UFL/OFL in a crate fail cargo-deny); Inter 4.1 is bundled instead.
  `deny.toml` checks Linux targets only.
- Flags0-6 bit meanings are NOT in ecmdroid.db (all names empty), so idle /
  closed loop / WOT flags are shown raw. Need a real source before naming
  them (KICKOFF §5 lists them).
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
  tracing-subscriber, libc, futures. Ask before adding any others.

## Next steps

1. Owner runs the phase 3 checklist below on the laptop and sends the CSV
   log(s) and anything that looks wrong (screenshots welcome).
2. Optional: btsnoop capture of EcmDroid reading the EEPROM / live data, to
   confirm frames byte for byte.
3. Phase 4 (only after the owner says go): diagnostics - trouble codes,
   actuator tests, TPS/AFV reset. Every actuator path tested against the sim
   first (KICKOFF §6.7); nothing that writes/actuates on the bike without
   sign-off.

## Phase 3 hardware checklist (read-only)

Bike on its stand, somewhere ventilated. Snap: build here, install on the
laptop with `sudo snap install --dangerous xbtune_*.snap`; check
`snap connections xbtune` (bluez, raw-usb, removable-media connected).
1. Ignition ON, engine OFF: `xbtune.cli live --bt 00:11:22:33:44:55`.
   Expect RPM 0, sensible engine/air temp, battery ~12-13 V, ~12-16 Hz,
   0% errors. Compare a few values with EcmDroid's live screen.
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
