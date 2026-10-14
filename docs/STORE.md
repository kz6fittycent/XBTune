# Snap Store notes

Nothing is uploaded until the repo can go public (KICKOFF §2 and the
ecmdroid.db question in `PROVENANCE.md`). The publishing workflows are
switched off by the `XBTUNE_STORE_PUBLISH` repository variable.

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
> Source: <repo URL once public>

## Listing (draft)

- **Summary:** Diagnostics, logging and tuning for Buell XB motorcycles
- **Categories:** Utilities, Development
- **Screenshots:** dashboard, fuel map editor, log viewer (phase 8)
