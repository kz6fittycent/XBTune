# XBTune

Diagnostics, logging and tuning for the DDFI-2 and DDFI-3 ECMs on 2003–2009
Buell XB9 and XB12 motorcycles, for Linux. Bluetooth (BuellTooth and other
SPP adapters) is the main link. USB serial cables work too.

**Status: phase 1, read-only.** XBTune can identify the ECM and measure the
link. It never writes to the ECM.

```
xbtune.cli doctor                       # snap connections and paths
xbtune.cli scan                         # Bluetooth adapters
xbtune.cli ports                        # USB serial adapters
xbtune.cli info --bt 00:11:22:AA:BB:CC  # identify the ECM over Bluetooth
xbtune.cli info --serial /dev/ttyUSB0   # ... or USB
xbtune.cli info --sim                   # against the built-in simulator
```

## Credits and license

The protocol code is a Rust port of [EcmDroid](https://github.com/ecmdroid/ecmdroid)
by Michel Marti (GPL-3.0). See `docs/PROVENANCE.md`.

XBTune is licensed GPL-3.0-or-later. See `LICENSE`.
