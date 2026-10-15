# File formats

## `.xbt`: XBTune EEPROM image (version 1)

A UTF-8 TOML file. Example:

```toml
# XBTune EEPROM image. Format: docs/FILE_FORMATS.md
format = "xbtune-eeprom"
format_version = 1
ecm_id = "BUEIB"
ecm_version = "BUEIB310 12-11-03"
length = 1210
page_zero = true
crc32 = "d52bbe25"
data = """
0000: 00 00 00 00 32 4d 38 04 7b 06 9e 00 cc 00 f4 01
...
"""

[meta]
created = "2026-10-14T19:44:19Z"
source = "read from ECM over Bluetooth 00:11:22:33:44:55 channel 1, every chunk read twice and matched"
vin = ""
notes = "stock"
```

| Key | Meaning |
|---|---|
| `format` | Always `xbtune-eeprom`. |
| `format_version` | 1. Readers refuse newer versions. |
| `ecm_id` | Five-character ECM ID. Must be in the catalog. |
| `ecm_version` | Full version string from the ECM (optional). |
| `length` | Number of bytes; always the ECM's `xsize`. |
| `page_zero` | `true` if the last `xsize - size` bytes are page 0 data read from an ECM; `false` if they are zero padding. Variables in page 0 are not decoded when false. |
| `crc32` | CRC-32 (IEEE, as zlib) of the bytes, 8 lower-case hex digits. A mismatch refuses the file. |
| `data` | The bytes as hex, 16 per line, each line prefixed `oooo:` (offset, ignored on read). Whitespace is free. |
| `[meta]` | Free text: `created` (RFC 3339 UTC), `source`, `vin`, `notes`. Safe to edit. |

The byte layout is the flat image EcmDroid uses: pages 1..N back to back
from offset 0 in page-number order, then page 0 at the end.

Files are written to a temporary name and renamed, so a crash never leaves a
half-written image.

### Edit metadata

Saving an edited tune adds two optional `[meta]` keys (older files without
them still load):

- `modified`: when it was last edited in XBTune (RFC 3339 UTC).
- `parent_crc32`: CRC-32 of the image it was opened from, so an edited tune
  can be traced back to the original read.

An image read from the ECM (`source` starts with "read from ECM") and never
edited is a backup: the app saves edits to it under a new name.

## `.xpr`: EcmDroid raw image

Just the bytes, no header. EcmDroid writes the whole buffer (`xsize`, page 0
last) and accepts either `size` or `xsize` bytes on load, working out the ECM
type from the length. Its file names start with the ID
(`BUEIB_20261014-194005.xpr`).

XBTune reads `.xpr` with the ECM ID from `--id`, else the file name prefix,
else the length (only if every matching ID shares one definition). It
writes `xsize` bytes when page 0 is real, otherwise only `size`, so padding
is never mistaken for page 0 data.

## `.csv`: XBTune live log (`xbtune-log 1`)

Written by `xbtune-cli log` and the app. A block of `#` lines, one column
row, then one row per runtime frame:

```text
# XBTune live log
# format: xbtune-log 1
# app: xbtune 0.1.0
# ecm_id: BUEIB
# version: BUEIB310 12-11-03
# target: Bluetooth 00:11:22:33:44:55 channel 1
# started: 2026-10-14T21:00:00Z
# note: first ride            (only if given; newlines become spaces)
# channel: RPM | Engine Speed | RPM
# channel: pw1 | Fuel Pulsewidth Front | ms
...
time_s,SysConfig,MilliSec,Seconds,RPM,spark1,...,frame
0.185,123,0.35,0,1110,16.17,...,0110003D02065...
```

- `time_s`: seconds since the session started polling (not since the log
  started), three decimals. Rows are in time order; gaps mean dropouts.
- One column per channel of the definition, in frame order, named by
  EcmDroid channel name (`# channel:` lines give label and units).
  Scalars are in engineering units with the channel's decimals (4 if the
  definition gives none); bitfields are the byte as a decimal number.
  An empty cell means the frame was too short for that channel.
- `frame`: the whole runtime reply frame as uppercase hex, so a log can be
  decoded again later with corrected definitions.
- Columns are separated by `,`; names containing `,` or `"` are quoted.
- `# event: t=<s> <text>` lines may appear between rows: failed attempts
  ("1 failed attempt(s) (timeout 1)"), requests that gave up, reconnects.
  `t` uses the same clock as `time_s`. Readers skip every `#` line.
- The file is flushed about once a second and when logging stops.
