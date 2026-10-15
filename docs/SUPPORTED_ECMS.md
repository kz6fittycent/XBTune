# Supported ECMs

Built from EcmDroid's `ecmdroid.db` (`eeprom` and `pages` tables) at commit
`2f17d29f`. Bike fitment comes from public pages (see `docs/PROVENANCE.md`,
sources RM and BX). The live list is `xbtune-cli ecms`.

**First hardware target:** the owner's 2005 XB9SX. This should report
**BUEIB** (DDFI-2). ECMs are sometimes swapped, so `xbtune-cli info` shows
what the bike really has.

## In scope

| ECM ID | Family | Bikes | Definition | EEPROM (no page 0 / with page 0) | Pages | Runtime frame | Notes |
|---|---|---|---|---|---|---|---|
| BUECB | DDFI-2 | XB9 2003 | BUECB | 1138 / 1138 | 1–6 (no page 0) | 103 | |
| BUEGB | DDFI-2 | XB 2004 | BUEGB | 1198 / 1202 | 0–6 | 107 | |
| BUEIB | DDFI-2 | XB 2005–2007 | BUEIB | 1206 / 1210 | 0–6 | 107 | **Owner's bike (expected)** |
| BUEIC | DDFI-2 | XB 2005–2007, Japan | BUEIB | 1206 / 1210 | 0–6 | 107 | Shares BUEIB definitions |
| BUEOD | DDFI-3 | XB 2008+ | BUEOD | 2788 / 2820 | 0–11 | 135 | |
| BUEWD | DDFI-3 | XB or 1125, 2008+ | BUEWD | 3116 / 3156 | 0–13 | 135 | |
| BUEYD | DDFI-3 | XB or 1125, 2008+ | BUEYD | 3232 / 3272 | 0–13 | 135 | |
| BUEZD | DDFI-3 | XB or 1125, 2008+ | BUEZD | 3372 / 3412 | 0–14 | 135 | |
| BUE1D | DDFI-3 | XB or 1125, 2008+ | BUE1D | 3404 / 3444 | 0–14 | 135 | |
| BUE2D | DDFI-3 | XB or 1125, 2008+ | BUE2D | 3756 / 3796 | 0–15 | 135 | EcmDroid test image available |
| BUE3D | DDFI-3 | XB or 1125, 2008+ | BUE3D | 3884 / 3924 | 0–16 | 135 | |

### Definition data per ID (from `xbtune-cli defs <ID>`)

| ECM ID | EEPROM variables | Runtime variables | Data issues |
|---|---|---|---|
| BUECB | 214 | 67 | 1 unnamed variable |
| BUEGB | 233 | 70 | 2 unnamed |
| BUEIB / BUEIC | 241 | 70 | 3 unnamed (`KSparkCtrl`, `Pad2`, `Pad3`); no layout problems |
| BUEOD | 411 | 115 | `KVS_RPM_Revs` left out (conflicting rows); 1 unnamed |
| BUEWD | 519 | 115 | 2 |
| BUEYD | 533 | 115 | 3, incl. `Tab_Idle_Spark` layout doesn't fit (raw bytes only) |
| BUEZD | 539 | 112 | 2 |
| BUE1D | 537 | 111 | 2, incl. `Tab_Idle_Spark` layout doesn't fit (raw bytes only) |
| BUE2D | 544 | 111 | 1 |
| BUE3D | 662 | 111 | 19 unnamed (many look 1125-specific) |

"Unnamed" means the variable has no `names` row; EcmDroid hides these.
XBTune keeps them, labelled with the variable name.

## Race ECMs (supported, untested on hardware)

Factory race ECMs answer only on address 0x55 at 19200 baud. Choose
**ECM type -> Factory race** on the app's Connect screen (CLI: `--race`)
before connecting; the wrong choice looks like a silent ECM, and XBTune
says so. This follows EcmDroid's "Stock / P&A" vs "Factory Race" protocol
choice (commit `3c189be`) and its USB baud fix (`2f17d29`).

- **Factory race:** RACE USE ONLY *engraved* on the case.
- **P&A race:** RACE USE ONLY *printed* on the case. These talk like stock
  ECMs: choose "Stock".
- **USB cable:** XBTune opens the port at 19200 for factory race.
- **Bluetooth:** the adapter's own serial side must be set to 19200 baud,
  8N1, no handshake. A BuellTooth is normally 9600; XBTune can't change it.

They use the street definitions below. Reading, live data, logging,
diagnostics and burns are tested against the simulator (a B2RIB) only.
No one has tried a real race ECM yet; reports welcome.

| ECM ID | Family | Bikes | Definition |
|---|---|---|---|
| B2RIB | DDFI-2 | XB 2005–2007, factory race | BUEIB |
| B3R1D | DDFI-3 | XB or 1125 2008+, EBR race | BUE1D |
| B3R2D | DDFI-3 | XB or 1125 2008+, EBR race | BUE2D |
| B3R3D | DDFI-3 | XB or 1125 2008+, EBR race | BUE3D |

Whether the EBR race IDs (B3R*D) use the race address or the stock one
isn't recorded anywhere we can cite: try "Factory race" first, then "Stock".

## Out of scope

- DDFI (tube frame, 1999–2002): BUEGC, BUEIA, BUEJA, BUEKA.
- The 1125. It shares DDFI-3 IDs with 2008+ XBs, but has a different
  engine and the definitions may cover XB-only and 1125-only variables.
  XBTune does not try to tell the two apart.

## Open questions

- Which 2008+ XB model and year shipped with which DDFI-3 ID? Neither
  ecmdroid.db nor the public sources map them one to one. This needs
  owner reports or a primary source.
- All writes stay disabled on every ID until phase 6 sign-off.
