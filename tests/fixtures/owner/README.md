# Owner's bike fixtures

Golden data from the first test bike (2005 XB9SX, ECM BUEIB). Private: this
is the owner's own calibration.

| File | How it is made |
|---|---|
| `xbtune.xbt` | `xbtune.cli dump --bt <ADDR> -o xbtune.xbt` (verified read) |
| `ecmdroid.xpr` | EcmDroid: EEPROM → Fetch, then Save, from the same ECM, same day |
| `ecmdroid_values.toml` | Values read off EcmDroid's Setup screens (see the test for the format) |

`crates/xbtune-core/tests/owner_golden.rs` checks that both images are
identical byte for byte and that XBTune decodes the listed values to what
EcmDroid shows. It is skipped (with a message) until the files exist.
