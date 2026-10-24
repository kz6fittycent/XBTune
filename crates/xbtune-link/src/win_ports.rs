// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows: what each COM port is, so a paired Bluetooth adapter can be
//! picked out of a list of bare `COMn` names.
//!
//! Windows makes two COM ports for a paired SPP device. Their device
//! instance IDs look like
//! `BTHENUM\{00001101-0000-1000-8000-00805F9B34FB}_LOCALMFG&0002\7&1A2B3C4D&0&001122334455_C00000000`:
//! the last `&` field holds the remote address on the Outgoing port (the
//! one that connects to the adapter) and `000000000000` on the Incoming
//! one (which waits for the adapter to call the PC).

use crate::BtAddress;

/// What a COM port is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortKind {
    /// Outgoing Bluetooth port to `address`; `name` as paired (`BuellTooth`).
    BluetoothOut { address: BtAddress, name: Option<String> },
    /// Incoming Bluetooth port: never answers when XBTune connects.
    BluetoothIn,
    /// Anything else, with Windows' name for it ("USB Serial Port (COM3)").
    Other { friendly: Option<String> },
}

/// Bluetooth side of a device instance ID, `None` if it isn't Bluetooth.
pub fn bluetooth_side(instance_id: &str) -> Option<Option<BtAddress>> {
    if !instance_id.to_ascii_uppercase().starts_with("BTHENUM\\") {
        return None;
    }
    let last = instance_id.rsplit('\\').next()?;
    let field = last.rsplit('&').next()?;
    let hex = field.split('_').next()?;
    if hex.len() != 12 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(None);
    }
    let mut a = [0u8; 6];
    for (i, b) in a.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some((a != [0; 6]).then_some(BtAddress(a)))
}

/// Every present COM port by name (`COM5`).
#[cfg(windows)]
pub fn ports() -> std::collections::HashMap<String, PortKind> {
    ffi::ports()
}

#[cfg(windows)]
mod ffi {
    use std::collections::HashMap;
    use std::ptr;

    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        DICS_FLAG_GLOBAL, DIGCF_PRESENT, DIREG_DEV, MAX_DEVICE_ID_LEN, SP_DEVINFO_DATA, SPDRP_FRIENDLYNAME,
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
        SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDevRegKey,
    };
    use windows_sys::Win32::Foundation::{FALSE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_BINARY, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    use windows_sys::core::GUID;

    use super::{PortKind, bluetooth_side};
    use crate::BtAddress;

    /// Device setup class "Ports (COM & LPT)".
    const GUID_DEVCLASS_PORTS: GUID = GUID::from_u128(0x4d36e978_e325_11ce_bfc1_08002be10318);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    pub fn ports() -> HashMap<String, PortKind> {
        let mut out = HashMap::new();
        // SAFETY: every pointer passed below is to a live local buffer with its
        // true length; the device list is destroyed once, at the end.
        unsafe {
            let hdi = SetupDiGetClassDevsW(&GUID_DEVCLASS_PORTS, ptr::null(), 0, DIGCF_PRESENT);
            if hdi == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut idx = 0;
            loop {
                let mut dev: SP_DEVINFO_DATA = std::mem::zeroed();
                dev.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
                if SetupDiEnumDeviceInfo(hdi, idx, &mut dev) == FALSE {
                    break;
                }
                idx += 1;
                let Some(port) = port_name(hdi, &dev) else { continue };
                let kind = match instance_id(hdi, &dev).as_deref().and_then(bluetooth_side) {
                    Some(Some(address)) => PortKind::BluetoothOut { address, name: paired_name(address) },
                    Some(None) => PortKind::BluetoothIn,
                    None => PortKind::Other { friendly: friendly_name(hdi, &dev) },
                };
                out.insert(port, kind);
            }
            SetupDiDestroyDeviceInfoList(hdi);
        }
        out
    }

    unsafe fn instance_id(hdi: isize, dev: &SP_DEVINFO_DATA) -> Option<String> {
        let mut buf = [0u16; MAX_DEVICE_ID_LEN as usize + 1];
        let mut needed = 0;
        // SAFETY: `buf` is writable for the length passed.
        let ok =
            unsafe { SetupDiGetDeviceInstanceIdW(hdi, dev, buf.as_mut_ptr(), buf.len() as u32, &mut needed) };
        (ok != FALSE).then(|| from_wide(&buf))
    }

    unsafe fn friendly_name(hdi: isize, dev: &SP_DEVINFO_DATA) -> Option<String> {
        let mut buf = [0u16; 256];
        let mut kind = 0;
        // SAFETY: `buf` is writable for the byte length passed.
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                hdi,
                dev,
                SPDRP_FRIENDLYNAME,
                &mut kind,
                buf.as_mut_ptr().cast(),
                (buf.len() * 2) as u32,
                ptr::null_mut(),
            )
        };
        (ok != FALSE && kind == REG_SZ).then(|| from_wide(&buf)).filter(|s| !s.is_empty())
    }

    unsafe fn port_name(hdi: isize, dev: &SP_DEVINFO_DATA) -> Option<String> {
        // SAFETY: `dev` came from SetupDiEnumDeviceInfo on `hdi`.
        let key = unsafe { SetupDiOpenDevRegKey(hdi, dev, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_READ) };
        if key == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut buf = [0u16; 64];
        let name = unsafe { query(key, "PortName", buf.as_mut_ptr().cast(), buf.len() * 2) };
        // SAFETY: `key` is open and closed once.
        unsafe { RegCloseKey(key) };
        match name {
            Some((REG_SZ, _)) => Some(from_wide(&buf)).filter(|s| s.starts_with("COM")),
            _ => None,
        }
    }

    /// The name the device gave when paired, from BTHPORT's registry key.
    fn paired_name(address: BtAddress) -> Option<String> {
        let a = address.0;
        let path = format!(
            "SYSTEM\\CurrentControlSet\\Services\\BTHPORT\\Parameters\\Devices\\{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            a[0], a[1], a[2], a[3], a[4], a[5]
        );
        let mut key: HKEY = 0;
        // SAFETY: `path` is NUL-terminated UTF-16; `key` is closed below.
        if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide(&path).as_ptr(), 0, KEY_READ, &mut key) } != 0 {
            return None;
        }
        let mut buf = [0u8; 256];
        let got = unsafe { query(key, "Name", buf.as_mut_ptr(), buf.len()) };
        unsafe { RegCloseKey(key) };
        match got {
            // UTF-8, usually NUL-terminated.
            Some((REG_BINARY, n)) => {
                let raw = &buf[..n.min(buf.len())];
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                Some(String::from_utf8_lossy(&raw[..end]).trim().to_string()).filter(|s| !s.is_empty())
            }
            _ => None,
        }
    }

    /// Read value `name` of `key` into `buf`: (type, bytes written).
    unsafe fn query(key: HKEY, name: &str, buf: *mut u8, len: usize) -> Option<(u32, usize)> {
        let mut kind = 0;
        let mut n = len as u32;
        // SAFETY: the caller's `buf` is writable for `len` bytes.
        let rc = unsafe { RegQueryValueExW(key, wide(name).as_ptr(), ptr::null(), &mut kind, buf, &mut n) };
        (rc == 0).then_some((kind, n as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_outgoing_from_incoming() {
        let out = r"BTHENUM\{00001101-0000-1000-8000-00805F9B34FB}_LOCALMFG&0002\7&1A2B3C4D&0&001122AABBCC_C00000000";
        let inc = r"BTHENUM\{00001101-0000-1000-8000-00805F9B34FB}_LOCALMFG&0000\7&1A2B3C4D&0&000000000000_00000000";
        assert_eq!(bluetooth_side(out), Some(Some(BtAddress([0x00, 0x11, 0x22, 0xAA, 0xBB, 0xCC]))));
        assert_eq!(bluetooth_side(inc), Some(None));
        assert_eq!(bluetooth_side(r"FTDIBUS\VID_0403+PID_6001+A702TB52A\0000"), None);
        assert_eq!(bluetooth_side(r"USB\VID_1A86&PID_7523\5&1E2C3D4F&0&2"), None);
    }
}
