# XBTune

Diagnostics, logging and tuning for the DDFI-2 and DDFI-3 ECMs on 2003–2009
Buell XB9 and XB12 motorcycles, for Linux. Bluetooth (BuellTooth and other
SPP adapters) is the main link. USB serial cables work too.

**Status: phase 7.** XBTune identifies the ECM, reads and saves the
EEPROM, shows and logs live data, reads trouble codes and edits tunes offline, from the command
line or the desktop app (`xbtune`). Active tests, clearing codes and TPS
reset, and writing tunes to the ECM (burn: backup, review, write only what
changed, read back), are each locked on real hardware until the owner
unlocks them.

```
xbtune.cli doctor                       # snap connections and paths
xbtune.cli scan                         # Bluetooth adapters
xbtune.cli ports                        # USB serial adapters
xbtune.cli info --bt 00:11:22:AA:BB:CC  # identify the ECM over Bluetooth
xbtune.cli info --serial /dev/ttyUSB0   # ... or USB
xbtune.cli info --sim                   # against the built-in simulator
xbtune.cli dump --bt ADDR -o tune.xbt   # read and save the EEPROM (verified)
xbtune.cli show tune.xbt Tab_Fuel_Front # decode an image
xbtune.cli live --bt ADDR               # live data in the terminal
xbtune.cli log --bt ADDR -o ride.csv    # log live data to CSV
xbtune.cli codes --bt ADDR              # current and stored trouble codes
xbtune.cli set tune.xbt KRPM_Hard_Hi=7450 -o new.xbt   # offline edit
xbtune.cli burn --bt ADDR new.xbt --vars KRPM_Hard_Hi   # write (locked until unlocked)
xbtune.cli logstats ride.csv            # log summary, O2 statistics, anomalies
xbtune.cli autotune --tune tune.xbt ride.csv -o tuned.xbt   # EGO auto-tune (file only)
xbtune                                  # the desktop app
```

The app opens with a short **Getting started** guide (tick "Don't show this at
startup" to turn it off; it stays in the side bar). **Report a problem** in the
side bar sends a report with the app's recent log to the developer as a GitHub
issue, without needing a GitHub account. If the app crashes, it offers to send a
report the next time it starts. Nothing is sent unless you click Send. The relay
is in `relay/bug-report/`.

## Credits and license

The protocol code is a Rust port of [EcmDroid](https://github.com/ecmdroid/ecmdroid)
by Michel Marti (GPL-3.0). See `docs/PROVENANCE.md`.

XBTune is licensed GPL-3.0-or-later. See `LICENSE`.
