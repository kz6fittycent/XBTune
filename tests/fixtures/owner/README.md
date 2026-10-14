# Owner's bike fixtures

Golden data from the first test bike (2005 XB9SX, ECM BUEIB). Private: this
is the owner's own calibration.

| File | How it is made |
|---|---|
| `BUEIB_xbtune.xpr` (or `xbtune.xbt`) | `xbtune.cli dump --bt <ADDR> --xpr BUEIB_xbtune.xpr` (verified read) |
| `BUEIB_20261014-162837.xpr` (or `ecmdroid.xpr`) | EcmDroid: EEPROM → Fetch, then Save, from the same ECM, same day |
| `idle.csv`, `BUEIB_20261014-231006.csv` | Live logs from the bike, engine running (`xbtune.cli log` and the app's Record), snap git a1bd3e7 |
| `ecmdroid_values.toml` | Values read off EcmDroid's Setup screens (see the test for the format) |

`crates/xbtune-core/tests/owner_golden.rs` checks that both images are
identical byte for byte (CRC-32 3b54974e), that the front fuel and spark
maps decode as they did on the bike, and that XBTune decodes the listed values to what
EcmDroid shows. It is skipped (with a message) until the files exist.
