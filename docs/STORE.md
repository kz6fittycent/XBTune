# Snap Store notes

The name `xbtune` was registered by the owner on 2026-10-14. On
2026-10-22 the repo went public and publishing was switched on
(`XBTUNE_STORE_PUBLISH=true`, secret `SNAPCRAFT_STORE_CREDENTIALS`,
environment `promote-stable` with the owner as required reviewer): every
green "🧪 Snap Builds" run on `main` uploads to `latest/candidate` and
opens a testing issue; `/promote <rev> stable` on it, plus approval,
releases to stable.

## Auto-connect request (draft, for the snapcraft forum "store-requests" category)

> **Title:** Auto-connect request: xbtune — bluez, raw-usb
>
> XBTune (snap `xbtune`) is an open-source (GPL-3.0) diagnostics and tuning
> tool for Buell XB motorcycles. It talks to the bike's engine control unit
> through a diagnostic adapter, either:
>
> - a **Bluetooth Classic SPP adapter** (BuellTooth and similar). XBTune
>   opens an RFCOMM socket and uses BlueZ over D-Bus to list and pair
>   adapters, so it needs **`bluez`**;
> - a **USB serial cable** (FTDI, CH340, CP210x), seen as `/dev/ttyUSB*` or
>   `/dev/ttyACM*`, so it needs **`raw-usb`**. `serial-port` is not an option,
>   because it needs a gadget/core slot, and these are ordinary USB cables
>   plugged into a laptop.
>
> Talking to the adapter is the app's only job. Without one of these
> connections it can do nothing except open saved files. Users are mostly
> motorcycle owners in a garage, not Linux experts, and a manual
> `snap connect` step is the most common reason they report it as broken.
>
> Like comparable tools in the store, it uses no other privileged
> interfaces. It does not use `bluetooth-control`.
>
> Source: https://github.com/kz6fittycent/XBTune

## Listing

XBTune is free (owner's decision, 2026-10-19): no paid tier, no ads, no
account. The store description is `description:` in `snap/snapcraft.yaml`
(it carries the engine-damage warning). Fields to set on the store page:

- **Title:** XBTune
- **Summary:** Diagnostics, logging and tuning for Buell DDFI motorcycles
- **Categories:** Utilities (primary), Development
- **License:** GPL-3.0-or-later
- **Contact:** https://github.com/kz6fittycent/XBTune/issues (`contact:` in `snapcraft.yaml`; never a personal email)
- **Website / source:** the repo URL once public (bug reports go through
  the app's Report a problem; see `relay/bug-report/`)
- **Icon:** none (owner's call: the Pegasus logo is a Buell trademark). The
  store shows a generic icon until one is made.
- **Screenshots** (1280x720-ish, dark theme, demo ECM, made with the
  screenshot hooks so no real data shows):
  1. Dashboard with the wideband gauge:
     `XBTUNE_SCREENSHOT=dash.ppm XBTUNE_VIEW=dashboard XBTUNE_SHOT_WIDEBAND=1 XBTUNE_SHOT_AFTER=12 xbtune`
  2. Fuel map with the log overlay:
     `XBTUNE_SCREENSHOT=map.ppm XBTUNE_VIEW=tune XBTUNE_TUNE=<demo dump> XBTUNE_SHOT_LOGS=<demo log> XBTUNE_SHOT_OVERLAY=afr XBTUNE_SHOT_TUNE="var=Tab_Fuel_Front" xbtune`
  3. Auto-tune review: as 2 plus `XBTUNE_SHOT_AUTOTUNE=1`
  4. Charts: `XBTUNE_VIEW=charts`
  5. Burn review: `XBTUNE_SHOT_BURN=KRPM_Hard_Hi=7450`
  Demo dump and log: `xbtune.cli dump --sim -o demo.xbt`,
  `xbtune.cli log --sim --wideband sim --duration 120 -o demo.csv`.
- **Grade/channel:** `grade: devel` until release; first upload to `edge`,
  then candidate via `call-for-testing.yml`, stable via
  `promote-to-stable.yml` (gated by `XBTUNE_STORE_PUBLISH`).

## Before the first upload

1. DONE 2026-10-22: the ecmdroid.toml licence answer (PROVENANCE).
2. DONE 2026-10-22: repo public.
3. DONE 2026-10-22: `grade: stable` in `snapcraft.yaml`.
4. Screenshots: made for the wiki (`images/` in the wiki repo) and the
   README (`docs/screenshots/`); upload some on the store page by hand.
5. Post the auto-connect request above; until it's granted the Getting
   started guide shows the `snap connect` commands.
