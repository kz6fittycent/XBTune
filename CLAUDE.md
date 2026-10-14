# XBTune: working notes

The brief is `docs/KICKOFF.md`. Read it first. Its rules on clean-room work
(§2), write safety (§6) and phases (§8) are non-negotiable.

## Status

- **Phase 1 (foundation): built and waiting for the owner's hardware test.**
  Don't start phase 2 until the owner says so.
- Owner's bike: **2005 Buell XB9SX**, so expected ECM **BUEIB** (DDFI-2).
  Not yet confirmed on hardware.
- Repo is private, with no remote. Don't add a remote or push without asking.
  Don't publish to the store.

## Layout

```
crates/xbtune-proto  PDU framing, checksum, decoder, EEPROM page model, transaction rules (no I/O)
crates/xbtune-link   serial + RFCOMM transports (BoxIo), EcmClient (retries/stats), BlueZ scan
crates/xbtune-defs   ECM catalog loaded from data/ecmdroid.toml + data/ecm_models.toml
crates/xbtune-core   env: snap detection, $SNAP_USER_COMMON paths, plug checks
crates/xbtune-sim    simulated ECM + fault injection; in-process and pty; bin xbtune-sim
crates/xbtune-cli    xbtune-cli: info, ports, scan, ecms, doctor
crates/xbtune-app    placeholder (egui app is phase 3)
data/                ecmdroid.toml (ED-DB derived, RESTRICTED), ecm_models.toml (ours)
tools/               extract_ecmdroid_db.py
tests/fixtures/      EcmDroid test images and runtime frame
snap/snapcraft.yaml  core24, rust plugin, apps: cli, sim
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
snapcraft pack && sudo snap install --dangerous xbtune_*.snap
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
- Bluetooth uses a direct RFCOMM socket (default channel 1, `--channel`).
  Discovery and pairing state come over D-Bus (bluer). No SDP lookup yet;
  if BuellTooth isn't on channel 1, add SDP via a BlueZ Profile.
- Snap stores everything under `$SNAP_USER_COMMON` (`xbtune_core::env::Dirs`).
- Snap apps: `xbtune.cli`, `xbtune.sim`. In phase 3 the GUI becomes `xbtune`,
  with the gnome extension.
- Publishing workflows are gated by the repo variable `XBTUNE_STORE_PUBLISH`.
- Approved extra deps (2026-10-14): clap, thiserror, anyhow, tracing,
  tracing-subscriber, libc, futures. Ask before adding any others.

## Next steps

1. Owner runs the phase 1 hardware checklist (below) over BuellTooth and USB,
   from the installed snap. Record the results here.
2. Ask for a btsnoop capture of EcmDroid connecting and reading the version.
   Confirm the version/runtime request frames, any echo, and the RFCOMM
   channel.
3. Phase 2 (only after the owner says go): extend the extractor to variables,
   tables and bits; full EEPROM read/save/open; `xbtune-cli dump`; golden
   test against the owner's dump; `docs/PARITY.md` from public ECMSpy material.

## Phase 1 hardware checklist (read-only)

Ignition ON, engine OFF, kill switch RUN, adapter on the diagnostic connector.
1. `snap connections xbtune`, then connect bluez, raw-usb, removable-media.
2. `xbtune.cli doctor`: all plugs connected, data dir under ~/snap/xbtune/common.
3. `xbtune.cli scan`: BuellTooth listed. Pair/trust it with bluetoothctl if needed.
4. `xbtune.cli info --bt <ADDR>`: expect `Version: BUEIB…`, `ECM ID: BUEIB (DDFI-2)`,
   `Runtime: 107-byte frame (matches definition)`, `State: idle`, few retries.
5. `xbtune.cli info --serial /dev/ttyUSB0` with a USB cable: same result.
6. Compare the version string with EcmDroid's home screen.
