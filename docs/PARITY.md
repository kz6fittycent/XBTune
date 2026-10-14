# Feature parity checklist

What the reference Windows tool for these ECMs does, learned **only** from its
public website and guides (KICKOFF §2 forbids anything else). Each row says
where XBTune covers it. Items marked **NEW** were not in KICKOFF §5.

Sources (read 2026-10-14): ecmspy.com home page; User Guide 2.0 sections
`menu`, `liveandlogdata`, `diagnostics`, `maps`, `eeprom`, `tuning`
(`/ug_mono/V2.0/…`); Tuning Guide v2 (`/tgv2/guide2.shtml`).

Status: ✅ done · 🟡 partial · ⬜ planned · ❌ deliberately not doing

## Connection and info

| Feature | XBTune | Phase |
|---|---|---|
| Pick port / adapter, ECM type detected from version | ✅ `info`, `scan`, `ports` | 1 |
| Unknown ECM type → clear error, point to supported list | ✅ read-only, writes refused | 1 |
| Fetch EEPROM on connect, with progress | 🟡 `dump` (CLI); app in 5 | 2/5 |
| Online / offline modes (edit an in-memory copy offline) | ⬜ | 5 |
| Trouble codes work even without a definition for the ECM | ⬜ | 4 |
| Status-bar error indicator when a code is set | 🟡 current codes on the dashboard; header indicator in 4 | 3/4 |
| Supported-ECM list, per-ECM runtime/EEPROM info pages | 🟡 `ecms`, `defs`, `docs/SUPPORTED_ECMS.md` | 2 |

## Live data and logging

| Feature | XBTune | Phase |
|---|---|---|
| Overview/dashboard while polling; switch tabs while polling | ✅ app: dashboard, live data, charts; CLI `live` | 3 |
| Log to file, new timestamped file each time | ✅ CSV (`log`, app Record), raw frames kept | 3 |
| **NEW** Binary log format (compact) and EcmDroid binary log import | ⬜ | 3/7 |
| **NEW** MegaLogViewer `.msl` text log export | ⬜ | 3/7 |
| Log playback in real time, driving the live views | ⬜ | 7 |
| **NEW** Narrowband O2 statistics (% rich/lean/error, average rich/lean V) | ⬜ | 7 |
| **NEW** Log anomaly flags (temperature jumps > 3.5 °C, gaps > 2 s, battery 11/16 V) | ⬜ | 7 |
| **NEW** Wideband O2 input (Innovate LC-1, PLX; up to two sensors) logged with ECM data | ⬜ needs a dependency + serial; ask first | 7 |
| **NEW** DDFI-3 analog wideband input configuration (0–5 V → AFR) | ⬜ definitions | 5 |

## Diagnostics

| Feature | XBTune | Phase |
|---|---|---|
| Fetch current and historic trouble codes | ⬜ (decoding tested in phase 2) | 4 |
| Clear historic codes (current ones stay while the fault exists) | ⬜ | 4 |
| **NEW** Error masks: choose which faults set a code and light the CEL (sensor/actuator/ECM) | ⬜ bit editors | 5 |
| Active tests (coils, injectors, pump, tach, fan, exhaust valve, …) with run-safety prompt | ⬜ | 4 |
| **NEW** LED-style input/output pin state indicators on the tests page | 🟡 DOut/DIn chips on the dashboard | 4 |
| **NEW** Static timing check aid | ⬜ protocol unknown; investigate | 4 |
| TPS reset (DDFI-2) with stored zero voltage / range shown | ⬜ | 4 |
| AFV reset ("Clear") | ⬜ | 4 |

## EEPROM and maps

| Feature | XBTune | Phase |
|---|---|---|
| Fetch, load, save EEPROM | ✅ `dump`, `.xbt`, `.xpr` | 2 |
| Hex/cell view with offset, byte, 16-bit LE value, variable name | 🟡 `show --list`, `diff` | 2/5 |
| Static maps (fuel, spark; idle setpoint on DDFI-3) | 🟡 view (`show`); edit in 5 | 5 |
| **NEW** Dynamic maps from logs: EGO, AFV, lambda, hits, weight | ⬜ | 7 |
| **NEW** Load/save individual maps to files | ⬜ | 5 |
| **NEW** Linked front/rear editing: unlocked, same difference, same value, same % | ⬜ | 5 |
| Scalars and flags (system, limits, corrections, O2 setup pages) | 🟡 view; edit in 5 | 5 |
| Burn full image or only changed cells; each write read back and compared | ⬜ changed pages only, always verified | 6 |
| Quick Burn of a single cell on Set (experimental there) | ❌ every burn goes through the §6 path | 6 |
| **NEW** Calibration string (8 chars) set before burn/save | ⬜ (`Cal_ID` exists on DDFI-3 IDs) | 5/6 |
| Compare edited copy against the fetched original | 🟡 `diff` | 2/5 |
| **NEW** Warn that DDFI-3 ECMs can lose changes if keep-alive power is faulty | ⬜ burn screen text | 6 |

## Tuning

| Feature | XBTune | Phase |
|---|---|---|
| EGO correction averaged per cell, hit counts (>100 significant) | ⬜ auto-tune proposes values | 7 |
| **NEW** Lambda (wideband) averaged per cell; front/rear separately | ⬜ | 7 |
| **NEW** Helper: multiply maps by the current AFV, then reset AFV | ⬜ | 7 |
| **NEW** Guided workflows ("Process Finder": six processes by sensor setup) | ⬜ docs + UI hints | 8 |
| Closed-loop / open-loop learn toggles, closed-loop boundaries | 🟡 scalars; edit in 5 | 5 |
| **NEW** Front cylinder correction table | 🟡 if in the definition | 5 |
