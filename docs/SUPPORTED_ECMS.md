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

## Recognised, not yet supported

Race ECMs use address 0x55 at 19200 baud (`--race`). They share street
definitions, but are not a target until there is hardware to test on.

| ECM ID | Family | Bikes | Definition |
|---|---|---|---|
| B2RIB | DDFI-2 | XB 2005–2007, factory race | BUEIB |
| B3R1D | DDFI-3 | XB or 1125 2008+, EBR race | BUE1D |
| B3R2D | DDFI-3 | XB or 1125 2008+, EBR race | BUE2D |
| B3R3D | DDFI-3 | XB or 1125 2008+, EBR race | BUE3D |

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
