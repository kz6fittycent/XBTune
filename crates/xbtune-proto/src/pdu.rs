// SPDX-License-Identifier: GPL-3.0-or-later
//! PDU (frame) layout, checksum and request builders.
//!
//! Ported from EcmDroid `app/src/main/java/org/ecmdroid/PDU.java`
//! (commit 2f17d29f5b08f67fb357ee8f2ec21e7ae930343e).
//! Copyright (C) 2012 by Michel Marti. Rust port Copyright (C) 2026 XBTune
//! contributors. Licensed GPL-3.0-or-later.
//!
//! Frame layout (all offsets from the start of the frame):
//!
//! ```text
//!  0    1       2          3      4    5    6 .. 6+n-1   6+n  7+n
//! SOH  sender  recipient  n+1    EOH  SOT  payload[n]    EOT  checksum
//! ```
//!
//! The length byte counts the payload plus the EOT byte. The checksum is the
//! XOR of every byte from `sender` up to and including EOT. A response
//! payload starts with a status byte: `ACK` (0x06) on success.

use std::fmt;

/// Start of header.
pub const SOH: u8 = 0x01;
/// End of header.
pub const EOH: u8 = 0xFF;
/// Start of text.
pub const SOT: u8 = 0x02;
/// End of text.
pub const EOT: u8 = 0x03;
/// Status byte of a successful response.
pub const ACK: u8 = 0x06;

/// Address of the tester (us).
pub const HOST_ID: u8 = 0x00;
/// Address of a stock (and P&A) ECM.
pub const STOCK_ECM_ID: u8 = 0x42;
/// Address of a factory race ECM.
pub const RACE_ECM_ID: u8 = 0x55;

/// Request runtime ("live") data.
pub const CMD_RTDATA: u8 = 0x43;
/// Write bytes to an EEPROM page, or trigger a function via page 0x20.
pub const CMD_SET: u8 = 0x57;
/// Read bytes from an EEPROM page.
pub const CMD_GET: u8 = 0x52;
/// Request the version string.
pub const CMD_VERSION: u8 = 0x56;

/// Page used for the busy/idle state byte and for function triggers.
pub const CONTROL_PAGE: u8 = 0x20;

/// Smallest possible frame: header (6) + EOT + checksum, no payload.
pub const MIN_FRAME_LEN: usize = 8;
/// Header length up to and including SOT.
pub const HEADER_LEN: usize = 6;
/// Largest frame: the length byte is one byte, so payload + EOT <= 255.
pub const MAX_FRAME_LEN: usize = 255 + 7;

/// Which ECM address (and baud rate) to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EcmAddress {
    /// Stock / P&A ECM: address 0x42, 9600 baud.
    #[default]
    Stock,
    /// Factory race ECM: address 0x55, 19200 baud.
    Race,
}

impl EcmAddress {
    pub const fn id(self) -> u8 {
        match self {
            Self::Stock => STOCK_ECM_ID,
            Self::Race => RACE_ECM_ID,
        }
    }

    /// Serial baud rate for this ECM type (EcmDroid `MainActivity.java`).
    pub const fn baud(self) -> u32 {
        match self {
            Self::Stock => 9600,
            Self::Race => 19200,
        }
    }
}

/// ECM functions: active tests and clearing codes. Sent as a SET to the
/// control page with the function code as the single data byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    ClearCodes = 0x01,
    FrontCoil = 0x02,
    RearCoil = 0x03,
    Tachometer = 0x04,
    FuelPump = 0x05,
    FrontInjector = 0x06,
    RearInjector = 0x07,
    TpsReset = 0x08,
    Fan = 0x09,
    ExhaustValve = 0x0A,
    ActiveIntake = 0x0B,
    ShiftLight = 0x0C,
}

impl Function {
    pub const fn code(self) -> u8 {
        self as u8
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::ClearCodes => "Clear Codes",
            Self::FrontCoil => "Front Coil",
            Self::RearCoil => "Rear Coil",
            Self::Tachometer => "Tachometer",
            Self::FuelPump => "Fuel Pump",
            Self::FrontInjector => "Front Injector",
            Self::RearInjector => "Rear Injector",
            Self::TpsReset => "TPS Reset",
            Self::Fan => "Fan",
            Self::ExhaustValve => "Exhaust Valve",
            Self::ActiveIntake => "Active Intake",
            Self::ShiftLight => "Shift Light",
        }
    }
}

/// Why a byte sequence is not a valid frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("frame too short ({0} bytes)")]
    Short(usize),
    #[error("frame does not start with SOH")]
    NoSoh,
    #[error("no EOH at offset 4")]
    NoEoh,
    #[error("no SOT at offset 5")]
    NoSot,
    #[error("length byte says {declared}, frame carries {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("no EOT before checksum")]
    NoEot,
    #[error("bad checksum: computed {computed:#04x}, received {received:#04x}")]
    Checksum { computed: u8, received: u8 },
}

/// XOR checksum over `frame[1..len-1]`, i.e. sender through EOT.
pub fn checksum(frame: &[u8]) -> u8 {
    if frame.len() < 2 {
        return 0;
    }
    frame[1..frame.len() - 1].iter().fold(0, |cs, b| cs ^ b)
}

/// A validated frame.
#[derive(Clone, PartialEq, Eq)]
pub struct Pdu {
    bytes: Vec<u8>,
}

impl Pdu {
    /// Build a frame from sender, recipient and payload.
    ///
    /// # Panics
    /// If the payload is longer than 254 bytes (the length byte overflows).
    pub fn new(sender: u8, recipient: u8, payload: &[u8]) -> Self {
        assert!(payload.len() < 255, "PDU payload too long");
        let mut bytes = Vec::with_capacity(payload.len() + MIN_FRAME_LEN);
        bytes.extend_from_slice(&[SOH, sender, recipient, payload.len() as u8 + 1, EOH, SOT]);
        bytes.extend_from_slice(payload);
        bytes.push(EOT);
        bytes.push(0);
        let cs = checksum(&bytes);
        *bytes.last_mut().expect("non-empty") = cs;
        Self { bytes }
    }

    /// Validate and wrap a complete frame.
    pub fn parse(frame: &[u8]) -> Result<Self, ParseError> {
        let n = frame.len();
        if n < MIN_FRAME_LEN {
            return Err(ParseError::Short(n));
        }
        if frame[0] != SOH {
            return Err(ParseError::NoSoh);
        }
        if frame[4] != EOH {
            return Err(ParseError::NoEoh);
        }
        if frame[5] != SOT {
            return Err(ParseError::NoSot);
        }
        let declared = frame[3] as usize;
        if n - 7 != declared {
            return Err(ParseError::LengthMismatch { declared, actual: n - 7 });
        }
        if frame[n - 2] != EOT {
            return Err(ParseError::NoEot);
        }
        let computed = checksum(frame);
        let received = frame[n - 1];
        if computed != received {
            return Err(ParseError::Checksum { computed, received });
        }
        Ok(Self { bytes: frame.to_vec() })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub fn sender(&self) -> u8 {
        self.bytes[1]
    }

    pub fn recipient(&self) -> u8 {
        self.bytes[2]
    }

    /// Payload, without EOT and checksum. For a response the first byte is
    /// the status (ACK or an error indicator).
    pub fn payload(&self) -> &[u8] {
        &self.bytes[HEADER_LEN..self.bytes.len() - 2]
    }

    /// First payload byte: the command of a request, or the status of a response.
    pub fn first_byte(&self) -> Option<u8> {
        self.payload().first().copied()
    }

    /// Data carried by a response, after the status byte.
    pub fn response_data(&self) -> &[u8] {
        self.payload().get(1..).unwrap_or(&[])
    }

    /// True if this frame was sent by `ecm` to the host.
    pub fn is_response_from(&self, ecm: EcmAddress) -> bool {
        self.sender() == ecm.id() && self.recipient() == HOST_ID
    }

    /// True if this frame was sent by the host to `ecm` (for example our own
    /// request echoed back by a half-duplex adapter).
    pub fn is_request_to(&self, ecm: EcmAddress) -> bool {
        self.sender() == HOST_ID && self.recipient() == ecm.id()
    }

    pub fn is_ack(&self) -> bool {
        self.first_byte() == Some(ACK)
    }
}

impl fmt::Debug for Pdu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pdu({})", hex(&self.bytes))
    }
}

/// Colon-separated upper-case hex, as EcmDroid's `Utils.hexdump`.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(':');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// A request the host can send. Knows its own frame and what a valid
/// response to it looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Read the version string, e.g. `"BUEIB310 12-11-03"`.
    Version,
    /// Read the runtime data block.
    RuntimeData,
    /// Read `len` (1..=16) bytes at `offset` within EEPROM page `page`.
    Get { page: u8, offset: u8, len: u8 },
    /// Write `data` at `offset` within EEPROM page `page`.
    Set { page: u8, offset: u8, data: Vec<u8> },
    /// Trigger a function (active test, clear codes, TPS reset).
    Command(Function),
}

impl Request {
    /// Busy/idle state: one byte from the control page. 0 means idle.
    pub const CURRENT_STATE: Self = Self::Get { page: CONTROL_PAGE, offset: 0, len: 1 };

    pub fn payload(&self) -> Vec<u8> {
        match self {
            Self::Version => vec![CMD_VERSION],
            Self::RuntimeData => vec![CMD_RTDATA],
            Self::Get { page, offset, len } => vec![CMD_GET, *offset, *page, *len],
            Self::Set { page, offset, data } => {
                let mut p = vec![CMD_SET, *offset, *page];
                p.extend_from_slice(data);
                p
            }
            Self::Command(f) => vec![CMD_SET, 0, CONTROL_PAGE, f.code()],
        }
    }

    pub fn to_pdu(&self, ecm: EcmAddress) -> Pdu {
        Pdu::new(HOST_ID, ecm.id(), &self.payload())
    }

    /// True if sending this request twice has the same effect as sending it
    /// once. Only idempotent requests may be retried automatically.
    ///
    /// A `Set` writes the same bytes again, so it is idempotent, but the
    /// write path still decides on its own whether to retry (it verifies by
    /// reading back). A `Command` fires an actuator and is never retried.
    pub fn is_idempotent(&self) -> bool {
        !matches!(self, Self::Command(_))
    }

    /// True if this request changes ECM state.
    pub fn is_write(&self) -> bool {
        matches!(self, Self::Set { .. } | Self::Command(_))
    }

    /// Number of data bytes a valid response must carry, if fixed.
    pub fn expected_data_len(&self) -> Option<usize> {
        match self {
            Self::Get { len, .. } => Some(*len as usize),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vectors from EcmDroid app/src/androidTest/java/org/ecmdroid/TestPDU.java.

    #[test]
    fn version_response() {
        let data = [
            0x01, 0x42, 0x00, 0x13, 0xFF, 0x02, 0x06, 0x42, 0x55, 0x45, 0x49, 0x42, 0x33, 0x31, 0x30, 0x20,
            0x31, 0x32, 0x2D, 0x31, 0x31, 0x2D, 0x30, 0x33, 0x03, 0xE2,
        ];
        let pdu = Pdu::parse(&data).unwrap();
        assert!(pdu.is_response_from(EcmAddress::Stock));
        assert!(!pdu.is_request_to(EcmAddress::Stock));
        assert!(pdu.is_ack());
        assert_eq!(pdu.sender(), STOCK_ECM_ID);
        assert_eq!(pdu.response_data(), b"BUEIB310 12-11-03");
    }

    #[test]
    fn set_request() {
        let req = Request::Set { page: 0x42, offset: 0x24, data: b"TestMe".to_vec() };
        let pdu = req.to_pdu(EcmAddress::Stock);
        assert!(pdu.is_request_to(EcmAddress::Stock));
        assert!(!pdu.is_ack());
        assert_eq!(pdu.payload()[1], 0x24); // offset
        assert_eq!(pdu.payload()[2], 0x42); // page
        assert_eq!(&pdu.payload()[3..], b"TestMe");
        assert!(Pdu::parse(pdu.as_bytes()).is_ok());
    }

    #[test]
    fn get_request() {
        let pdu = Request::Get { page: 1, offset: 0, len: 0x10 }.to_pdu(EcmAddress::Stock);
        assert_eq!(hex(pdu.as_bytes()), "01:00:42:05:FF:02:52:00:01:10:03:FA");
    }

    #[test]
    fn get_response() {
        let data = [
            0x01, 0x42, 0x00, 0x12, 0xff, 0x02, 0x06, 0x00, 0x00, 0x00, 0x01, 0x00, 0x4d, 0xe8, 0x03, 0x7b,
            0x06, 0x9e, 0x00, 0xcc, 0x00, 0xf4, 0x01, 0x03, 0xd5,
        ];
        let pdu = Pdu::parse(&data).unwrap();
        assert!(pdu.is_ack());
        assert_eq!(hex(pdu.response_data()), "00:00:00:01:00:4D:E8:03:7B:06:9E:00:CC:00:F4:01");
    }

    #[test]
    fn function_request() {
        let pdu = Request::Command(Function::Fan).to_pdu(EcmAddress::Stock);
        assert_eq!(hex(pdu.as_bytes()), "01:00:42:05:FF:02:57:00:20:09:03:C7");
    }

    #[test]
    fn version_and_runtime_requests() {
        // Derived from PDU.java's GET_VERSION / GET_RT construction.
        let v = Request::Version.to_pdu(EcmAddress::Stock);
        assert_eq!(hex(v.as_bytes()), "01:00:42:02:FF:02:56:03:E8");
        let rt = Request::RuntimeData.to_pdu(EcmAddress::Stock);
        assert_eq!(hex(rt.as_bytes()), "01:00:42:02:FF:02:43:03:FD");
        let race = Request::Version.to_pdu(EcmAddress::Race);
        assert_eq!(race.recipient(), RACE_ECM_ID);
        assert!(Pdu::parse(race.as_bytes()).is_ok());
    }

    #[test]
    fn current_state_request() {
        let pdu = Request::CURRENT_STATE.to_pdu(EcmAddress::Stock);
        assert_eq!(&pdu.payload(), &[CMD_GET, 0x00, 0x20, 0x01]);
    }

    #[test]
    fn runtime_fixture_parses() {
        let rt = include_bytes!("../../../tests/fixtures/RT_BUEIB.bin");
        let pdu = Pdu::parse(rt).unwrap();
        assert!(pdu.is_response_from(EcmAddress::Stock));
        assert!(pdu.is_ack());
        assert_eq!(rt.len(), 107); // poll_len for BUEIB in ecmdroid.db
    }

    #[test]
    fn parse_errors() {
        let good = Request::Version.to_pdu(EcmAddress::Stock).into_bytes();
        assert_eq!(Pdu::parse(&good[..5]), Err(ParseError::Short(5)));
        let mut bad = good.clone();
        bad[0] = 0x00;
        assert_eq!(Pdu::parse(&bad), Err(ParseError::NoSoh));
        let mut bad = good.clone();
        bad[4] = 0x00;
        assert_eq!(Pdu::parse(&bad), Err(ParseError::NoEoh));
        let mut bad = good.clone();
        bad[5] = 0x00;
        assert_eq!(Pdu::parse(&bad), Err(ParseError::NoSot));
        let mut bad = good.clone();
        bad[3] = 9;
        assert!(matches!(Pdu::parse(&bad), Err(ParseError::LengthMismatch { .. })));
        let mut bad = good.clone();
        let n = bad.len();
        bad[n - 1] ^= 0x55;
        assert!(matches!(Pdu::parse(&bad), Err(ParseError::Checksum { .. })));
        let mut bad = good;
        bad[n - 2] = 0x04;
        assert!(Pdu::parse(&bad).is_err());
    }

    #[test]
    fn roundtrip_all_payload_lengths() {
        for n in 0..254usize {
            let payload: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
            let pdu = Pdu::new(HOST_ID, STOCK_ECM_ID, &payload);
            let back = Pdu::parse(pdu.as_bytes()).unwrap();
            assert_eq!(back.payload(), &payload[..]);
        }
    }

    #[test]
    fn only_commands_are_non_idempotent() {
        assert!(Request::Version.is_idempotent());
        assert!(Request::CURRENT_STATE.is_idempotent());
        assert!(!Request::Command(Function::FuelPump).is_idempotent());
        assert!(Request::Command(Function::Fan).is_write());
        assert!(!Request::RuntimeData.is_write());
    }
}
