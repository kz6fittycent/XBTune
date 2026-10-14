# Prompt: XBTune, a Linux tuner for Buell XB motorcycles

Paste everything below the line into Claude Code, started in `~/git/XBTune`.
("XBTune" is a working name. Renaming it later is cheap.)

---

I want to build **XBTune**, a native Linux app for diagnosing, logging and tuning the
DDFI-2 and DDFI-3 ECMs on **2003–2009 Buell XB9 and XB12** motorcycles. It must do everything the
Windows app ECMSpy does for these bikes, with a focus on working reliably over
**Bluetooth**. I'll do my tuning with it, so correctness and write safety matter more
than anything else.

My priorities, in order:
1. **Never damage the ECM or the engine.** Every write is backed up, minimal, verified,
   and needs explicit confirmation.
2. **Feature parity with ECMSpy** for XB9/XB12 (section 5).
3. **A reliable link over Bluetooth.** Expect dropouts, latency and partial reads.
4. **A clean, modern UI.** It should look like a polished consumer app, not a dev tool.

## 1. Scope

**In scope:** every 2003–2009 XB model (XB9R, XB9S, XB9SX, XB12R, XB12S, XB12Ss,
XB12Scg, XB12STT, XB12X, XB12XT) on **both ECM families**:
- **2003–2007 XBs: DDFI-2.**
- **2008–2009 XBs: DDFI-3.** The DDFI-3 has its own firmware versions, its own EEPROM
  layout and size, and possibly different runtime variables and features. Treat
  DDFI-2 and DDFI-3 as two separate definition families that share one protocol stack.
  Check every difference against EcmDroid. Don't assume DDFI-2 behaviour carries over.

Build the exact list of supported ECM firmware/version IDs for each family from the
EcmDroid data (section 2). Don't guess it. Put the list, with ECM family and model
years, in `docs/SUPPORTED_ECMS.md`. My own bike is the first hardware target. Ask me
which model and year it is before phase 1 hardware testing. The other family is tested
against the simulator and EEPROM images until real hardware is available.

**Out of scope for now:** tube-frame DDFI-1 bikes, the 1125 (also DDFI-3, but a different
engine and different definitions), Blast, and Harley
ECMs. Keep the protocol and definition layers general so these can be added later
without rewrites. Don't build them.

## 2. Legal and clean-room rules (non-negotiable)

- **ECMSpy is off limits.** Its EULA forbids decompiling or reverse engineering it, and
  forbids using it to make a competing product. Never download, decompile, run, inspect
  or capture traffic from ECMSpy. Never use its definition files, map files or database.
  Learn *what* it does only from public material (website, forum posts, public guides and
  videos), and only to understand the feature list. Don't use the name "ECMSpy" in the
  app, its UI or its marketing.
- **EcmDroid is our reference implementation.** https://github.com/ecmdroid/ecmdroid is
  GPL-3.0, so we may port its protocol code (PDU framing, checksums, the ECM transaction
  layer, the EEPROM page model, variable offset/scaling resolution) into Rust. Credit
  EcmDroid's copyright in the file headers of ported modules, and cite the original Java
  file and commit.
- **`ecmdroid.db` is a special case.** EcmDroid says it was converted from a database dump
  that the ECMSpy maintainer gave permission to use under GPLv3. That's fine for private
  development and testing. Don't make the repo public or ship a release that contains
  that data until I confirm the permission covers us. Design the definitions so that this
  data lives in **one replaceable data file** with its own provenance note, never mixed
  into the code.
- Record the source of every protocol detail, offset and scaling in `docs/PROVENANCE.md`.
- License: **GPL-3.0-or-later**. Allowed dependency licenses are MIT, Apache-2.0, BSD,
  ISC, Zlib, MPL-2.0 and Unicode, enforced by `cargo-deny`.
- The repo stays **private** for now. Don't create remotes or push without asking me.

## 3. My setup

- Ubuntu 26.04, Linux only. I have no Windows machine.
- Interface: a **BuellTooth** adapter (Bluetooth Classic SPP) on the bike's diagnostic
  connector. Also support USB serial adapters (FTDI/CH340 → `/dev/ttyUSB*`) and BLE UART
  adapters (HM-10 style), but BuellTooth is the primary target and must work first.
- **I don't have a spare ECM.** Every write runs against the ECM on my own bike.
- I have EcmDroid on an Android phone. It's my fallback recovery tool and a cross-check
  for read values. I can also record Android "Bluetooth HCI snoop" logs of EcmDroid
  sessions and send them to you as protocol ground truth (parse btsnoop → RFCOMM payloads).

## 4. Tech stack (decided)

- **Rust** (stable, 2024 edition), Cargo workspace.
- **UI:** `egui`/`eframe` with a custom theme: good typography (Inter), consistent
  spacing, real gauge widgets, dark and light modes. Use `egui_plot` for charts. Draw the
  table heatmaps and the 3D map surface view ourselves with the egui painter.
- **Serial:** the `serialport` crate.
- **Bluetooth:** `bluer` (BlueZ). Open RFCOMM directly as a socket, so the user doesn't
  need `rfcomm bind` or root. Use GATT for BLE UART. Include device discovery and pairing
  status in the UI.
- **Async:** `tokio`. The ECM link runs on its own task. The UI never blocks on I/O.
- **Storage:** `serde` + `toml` for settings, XDG directories (`directories` crate).
  Logs are CSV with a header block of metadata.
- **Distribution: a snap (`xbtune`), strict confinement, from day one.** The snap is how
  the app ships and how I'll test it. Getting Bluetooth and serial working under strict
  confinement is a real risk, so prove it early. Don't leave it for the end.
  - `snap/snapcraft.yaml` goes in Phase 1. Use base `core24`, the Rust plugin, and the
    `gnome` extension (or the equivalent desktop/wayland/x11/opengl plugs) for the egui
    window.
  - Plugs: `bluez` (RFCOMM and BLE through BlueZ), `raw-usb` (USB serial adapters,
    `/dev/ttyUSB*` and `/dev/ttyACM*`), `home`, and `removable-media` (EEPROM images and
    logs on USB sticks). `~/git/TunerStudio-snap/snap/snapcraft.yaml` is my working
    reference for a tuning app that uses serial and Bluetooth in a snap.
  - `bluez` and `raw-usb` don't auto-connect. Detect missing connections at runtime and
    show a first-run screen with the exact `snap connect xbtune:...` commands. Draft the
    store auto-connect request text in `docs/STORE.md` for later.
  - Store config and data under `$SNAP_USER_COMMON`, so backups and tune history survive
    snap refreshes and reverts. Never use the per-revision `$SNAP_USER_DATA`.
  - CI: adapt my existing workflows in `~/git/Desktop-Scriptures/.github/workflows/`
    (`snap-builds.yml`, `call-for-testing.yml`, `promote-to-stable.yml`,
    `security-rebuild.yml`). Read them, keep their structure, and change only the names
    and paths. Until I say the repo can go public, build locally (`snapcraft pack`) and
    don't publish to the store.

Workspace layout. Only `xbtune-app` knows about egui:

```
crates/
  xbtune-proto     PDU framing, checksums, commands, transaction layer, retries (no I/O)
  xbtune-link      transports: serial, RFCOMM, BLE; one async trait; reconnect logic
  xbtune-defs      ECM definitions: versions, EEPROM layout, tables, scalars, units
  xbtune-core      session state, EEPROM image model, diffs, logging, auto-tune math
  xbtune-sim       simulated ECM that serves an EEPROM image + synthetic runtime data
  xbtune-app       egui UI
  xbtune-cli       headless tool: info, dump, logging, diff (handy over SSH and for tests)
```

## 5. Feature parity (XB9/XB12, DDFI-2 and DDFI-3)

Where a feature differs between the two families, put that difference in the
definitions, not in `if ddfi3` branches scattered through the UI.

**Connection and info**
- Scan for and pick a port or Bluetooth device. Remember the last one. Auto-reconnect.
- Read the ECM version/firmware ID, map it to a definition, and refuse writes on unknown
  versions (read-only mode).
- Show link quality: round-trip time, error and retry counts.

**Live data**
- Every runtime variable the definition exposes, for example RPM, TPS (raw/%), front and
  rear fuel pulse width, front and rear spark advance, engine temp, intake air temp,
  O2 sensor, EGO correction (front/rear), AFV, battery volts, VSS/speed, gear,
  idle/closed-loop/WOT/decel flags, and error flags. Show units and scaling.
- A dashboard of configurable gauges, plus a full variable table view.
- Charts over time with selectable channels.
- Highlight the live operating point on the fuel and spark tables.

**Logging**
- Record to CSV at the highest rate the link can sustain. Show the achieved rate.
- Log viewer: playback, scrubbing, channel charts, cell hit counts, and average EGO per
  fuel-table cell shown as an overlay heatmap.

**Diagnostics**
- Read current and historic trouble codes, with descriptions. Clear codes.
- Actuator tests the definition supports, for example front/rear injector, front/rear
  coil, fuel pump, tachometer, check engine light, fan, and exhaust valve where fitted.
  Each one needs a confirmation step and a "make sure the bike is safe to do this" warning.
- TPS reset/calibration, and AFV reset, following the documented procedure.

**EEPROM and maps**
- Read the full EEPROM with a progress bar. Retry each page and resume after a dropout.
- Save and open EEPROM images. Use our own documented format: raw bytes plus metadata
  (ECM ID, date, VIN/notes if I type them). Import and export the raw EEPROM image format
  EcmDroid uses, so I can move images between the two apps.
- Edit every table and scalar in the definition:
  - Fuel (front/rear) and spark (front/rear) maps as a grid editor with a heatmap, a 3D
    surface view, and multi-cell select with set, add, multiply, percent and smooth/
    interpolate operations.
  - Smaller 2D tables, such as warm-up/cranking/acceleration enrichment, idle, fan and
    temperature tables.
  - Scalars and flags, such as rev limit, idle RPM, fan temperatures, closed-loop
    settings, O2/EGO limits, and feature flags.
- Undo/redo, and copy/paste between cells and between tables.
- Compare two maps or images: side-by-side, plus a difference heatmap. Copy tables
  across.
- **Burn to ECM**, following section 6.

**Tuning**
- EGO-based auto-tune: from one or more logs, compute the weighted average EGO correction
  per fuel cell with minimum hit counts, filters (warm engine, steady state, not in
  transition or decel) and per-cell change limits. Propose new fuel values as a diff
  that I review and accept per cell. It only changes the working map, never the ECM.
- Notes and a tune history: every burn saves the before and after images, with a
  timestamp and my note.

When you research ECMSpy's public material, list any feature I've missed in
`docs/PARITY.md` as a checklist, so we can track parity.

## 6. Write safety (non-negotiable)

1. Before the first write in a session, read the full EEPROM and save a timestamped
   backup automatically. No backup, no write.
2. Only write to an ECM whose ID matches the image's ECM ID and a known definition.
3. Write **only the pages that changed**, then read each one back and verify it
   byte for byte.
4. Show the diff (tables and scalars, in human units) and require explicit confirmation
   before burning. Warn if the engine is running.
5. Bluetooth: check link health before burning (recent error rate, RTT). Refuse when it's
   poor, and suggest moving closer or switching to USB.
6. If a write fails partway, stop, keep the session open, report exactly which pages
   differ, and offer to retry or restore the backup.
7. All write and actuator code paths must have tests against `xbtune-sim`, including
   simulated dropouts, corrupted replies and timeouts, before they're enabled on real
   hardware.

## 7. Testing

- Unit tests for framing and checksums, using test vectors taken from EcmDroid's
  behaviour, and later from my HCI snoop logs.
- `xbtune-sim` speaks the same protocol over a virtual serial port (pty) and over an
  in-process transport. Most development and CI runs against it.
- Golden tests: decoding my real EEPROM dump must produce the same table values
  EcmDroid shows.
- **Hardware testing is my job.** At the end of each phase, give me a short checklist of
  what to run on the bike and what I should see. Start read-only. Don't enable writes on
  real hardware until I've signed off on the read path and on sim testing of the write
  path.

## 8. Phases

Stop at the end of each phase so I can test it. Commit after each working step with
clear messages. Don't start the next phase until I say so.

1. **Foundation.** Workspace, CI (fmt, clippy, tests, cargo-deny), `xbtune-proto`
   ported from EcmDroid with tests, `xbtune-sim`, and `xbtune-cli info` working against
   the sim, then against my bike over BuellTooth (RFCOMM) and USB. Also: the first
   `snapcraft.yaml`, with `xbtune-cli info` confirmed working over Bluetooth and USB
   **from inside the installed, strictly confined snap**, not just from `cargo run`.
2. **Definitions + read.** `xbtune-defs` loading the EcmDroid-derived data for in-scope
   DDFI-2 and DDFI-3 ECMs. Full EEPROM read, save and open. `xbtune-cli dump`. Golden test against my dump.
3. **Live data + logging.** Runtime polling, CLI logging, then the first egui app with
   connection, dashboard, variable table and charts.
4. **Diagnostics.** Trouble codes, actuator tests, TPS/AFV reset.
5. **Map editing (offline).** All table and scalar editors, compare and diff, undo/redo,
   working only on image files.
6. **Burn.** The section 6 write path: sim first, then on-bike with my sign-off.
7. **Log viewer + auto-tune.**
8. **Polish + release.** Theme, first-run flow including snap-connection checks, store
   listing and auto-connect request, CI promotion workflow, docs.

## 9. Working agreement

- Keep `CLAUDE.md` current with status, decisions and next steps, so a fresh session
  can pick up cold.
- When a protocol detail isn't clear, check EcmDroid's source first. Then ask me for an
  HCI snoop capture or a specific test on the bike. Don't guess on anything that writes.
- Ask before adding a dependency that isn't listed here, before any network-facing
  feature, and before anything that writes to the ECM on real hardware.
