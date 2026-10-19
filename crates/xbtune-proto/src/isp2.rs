// SPDX-License-Identifier: GPL-3.0-or-later
//! Innovate Serial Protocol 2 (ISP2, "MTS"): the data stream of Innovate
//! wideband controllers (LC-1, LC-2, LM-1/LM-2, and anything chained to
//! them). Not EcmDroid code: written from Innovate's published
//! "Innovate Serial Protocol 2 (ISP2) specification (preliminary)",
//! 5/3/2004 (see `docs/PROVENANCE.md`, "Wideband").
//!
//! 19200 baud, 8N1. A packet arrives about every 81.92 ms: a header word,
//! then `length` data words, all 16-bit big-endian. Both header bytes have
//! their high bit set; data bytes don't, except the first byte of an LM-1
//! sub-packet. XBTune only listens; it never sends commands.

/// Line speed of the MTS serial port.
pub const BAUD: u32 = 19_200;

/// A sensor's state, from the function bits of its first word.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    /// Lambda is valid.
    Normal,
    /// The value is the O2 level, percent (free air).
    O2Level(f64),
    FreeAirCalibration,
    NeedsFreeAirCalibration,
    /// Warming up; percent of operating temperature.
    WarmingUp(f64),
    /// Heater calibration; the value is a countdown.
    HeaterCalibration(u16),
    /// An error code (its meaning depends on the device).
    Error(u16),
    /// Function 7: flash level (LM-1) or reserved (LC-1).
    Other(u16),
}

impl Status {
    /// Short text without commas (used in CSV logs; [`Status::parse`] reads it back).
    pub fn text(&self) -> String {
        match self {
            Self::Normal => "ok".into(),
            Self::O2Level(p) => format!("o2 {p:.1}%"),
            Self::FreeAirCalibration => "calibrating".into(),
            Self::NeedsFreeAirCalibration => "needs calibration".into(),
            Self::WarmingUp(p) => format!("warming {p:.1}%"),
            Self::HeaterCalibration(n) => format!("heater calibration {n}"),
            Self::Error(n) => format!("error {n}"),
            Self::Other(n) => format!("other {n}"),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let num = |p: &str| s.strip_prefix(p).map(|r| r.trim().trim_end_matches('%'));
        Some(match s {
            "ok" => Self::Normal,
            "calibrating" => Self::FreeAirCalibration,
            "needs calibration" => Self::NeedsFreeAirCalibration,
            _ => {
                if let Some(v) = num("o2 ") {
                    Self::O2Level(v.parse().ok()?)
                } else if let Some(v) = num("warming ") {
                    Self::WarmingUp(v.parse().ok()?)
                } else if let Some(v) = num("heater calibration ") {
                    Self::HeaterCalibration(v.parse().ok()?)
                } else if let Some(v) = num("error ") {
                    Self::Error(v.parse().ok()?)
                } else {
                    Self::Other(num("other ")?.parse().ok()?)
                }
            }
        })
    }
}

/// One wideband sensor's reading (an LC-1/LC-2 or LM-1 sub-packet).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sensor {
    pub status: Status,
    /// Lambda, when `status` is `Normal`.
    pub lambda: Option<f64>,
    /// The device's stoichiometric AFR (multiplier / 10; 14.7 for petrol).
    pub stoich: f64,
}

impl Sensor {
    /// Air/fuel ratio for the device's fuel setting.
    pub fn afr(&self) -> Option<f64> {
        self.lambda.map(|l| l * self.stoich)
    }
}

/// One sensor-data packet.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Packet {
    /// A device in the chain is recording to its own memory.
    pub recording: bool,
    /// Wideband sensors, in chain order.
    pub sensors: Vec<Sensor>,
    /// Other channels (aux inputs, 10-13 bits), in chain order.
    pub aux: Vec<u16>,
}

/// Pulls packets out of a byte stream that may start mid-packet or carry
/// garbage. Command-response packets are skipped.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// Bytes thrown away while looking for a header.
    pub skipped: u64,
}

const HEADER_MASK: u16 = 0xA280; // bits 15, 13, 9, 7
const MAX_BUFFER: usize = 4096;

fn is_header(w: u16) -> bool {
    w & HEADER_MASK == HEADER_MASK
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns the sensor-data packets completed by them.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Packet> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            // Find a header: two bytes with their high bits set and bits
            // 13, 9 and 7 of the word set.
            let start = (0..self.buf.len().saturating_sub(1))
                .find(|&i| is_header(u16::from_be_bytes([self.buf[i], self.buf[i + 1]])));
            let Some(start) = start else {
                // Keep a possible first header byte.
                let keep = usize::from(self.buf.last().is_some_and(|b| b & 0x80 != 0));
                self.skipped += (self.buf.len() - keep) as u64;
                self.buf.drain(..self.buf.len() - keep);
                break;
            };
            self.skipped += start as u64;
            self.buf.drain(..start);
            let header = u16::from_be_bytes([self.buf[0], self.buf[1]]);
            let len = (usize::from(header >> 1) & 0x80) | usize::from(header & 0x7F);
            let total = 2 + 2 * len;
            if self.buf.len() < total {
                if self.buf.len() > MAX_BUFFER {
                    self.skipped += self.buf.len() as u64;
                    self.buf.clear();
                }
                break;
            }
            let words: Vec<u16> =
                self.buf[2..total].as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)).collect();
            // Data words never have bit 7 set; a header inside means this
            // one was a false start.
            if words.iter().any(|&w| w & 0x0080 != 0) {
                self.skipped += 1;
                self.buf.drain(..1);
                continue;
            }
            self.buf.drain(..total);
            if header & 0x1000 != 0 {
                out.push(parse_data(header, &words));
            }
        }
        out
    }
}

fn parse_data(header: u16, words: &[u16]) -> Packet {
    let mut p = Packet { recording: header & 0x4000 != 0, ..Default::default() };
    // The first wideband device's fuel setting applies to all of them.
    let mut stoich = None;
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let lm1 = w & 0x8000 != 0; // bits 13, 9, 7 are 0 here (not a header)
        let lc1 = w & 0xE200 == 0x4200; // 0 1 0 ... 1 in bits 15, 14, 13, 9
        if (lm1 || lc1) && i + 1 < words.len() {
            let mut s = sensor(w, words[i + 1]);
            let st = *stoich.get_or_insert(s.stoich);
            s.stoich = st;
            p.sensors.push(s);
            // LM-1: function, lambda, battery, then 5 aux words.
            if lm1 {
                p.aux.extend(words.iter().skip(i + 3).take(5).map(|&w| data13(w)));
                i += 8;
            } else {
                i += 2;
            }
        } else {
            p.aux.push(data13(w));
            i += 1;
        }
    }
    p
}

/// D12..D7 in bits 13..8, D6..D0 in bits 6..0.
fn data13(w: u16) -> u16 {
    ((w >> 1) & 0x1F80) | (w & 0x7F)
}

fn sensor(w0: u16, w1: u16) -> Sensor {
    let mult = ((w0 >> 1) & 0x80) | (w0 & 0x7F);
    let l = data13(w1);
    let v = f64::from(l);
    let status = match (w0 >> 10) & 7 {
        0 => Status::Normal,
        1 => Status::O2Level(v / 10.0),
        2 => Status::FreeAirCalibration,
        3 => Status::NeedsFreeAirCalibration,
        4 => Status::WarmingUp(v / 10.0),
        5 => Status::HeaterCalibration(l),
        6 => Status::Error(l),
        _ => Status::Other(l),
    };
    let lambda = (status == Status::Normal).then(|| (v + 500.0) / 1000.0);
    Sensor { status, lambda, stoich: f64::from(mult) / 10.0 }
}

/// An LC-1/LC-2 sub-packet: function bits 0..=7, the 13-bit lambda word
/// value, the AFR multiplier (stoich x 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lc1Words {
    pub function: u8,
    pub value: u16,
    pub multiplier: u8,
}

impl Lc1Words {
    /// Normal operation with `lambda` (0.5..=8.691), petrol.
    pub fn lambda(lambda: f64) -> Self {
        let value = ((lambda * 1000.0).round() - 500.0).clamp(0.0, 8191.0) as u16;
        Self { function: 0, value, multiplier: 147 }
    }
}

/// A sensor-data packet as a chain of LC-1/LC-2s sends it, then `aux`
/// words (for the simulator and tests).
pub fn encode(sensors: &[Lc1Words], aux: &[u16]) -> Vec<u8> {
    let len = (sensors.len() * 2 + aux.len()) as u16;
    let split13 = |v: u16| ((v & 0x1F80) << 1) | (v & 0x7F);
    let header = HEADER_MASK | 0x1000 | ((len & 0x80) << 1) | (len & 0x7F);
    let mut out = header.to_be_bytes().to_vec();
    for s in sensors {
        let m = u16::from(s.multiplier);
        let w0 = 0x4200 | (u16::from(s.function & 7) << 10) | ((m & 0x80) << 1) | (m & 0x7F);
        out.extend(w0.to_be_bytes());
        out.extend(split13(s.value).to_be_bytes());
    }
    for &a in aux {
        out.extend(split13(a).to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_lambda_examples() {
        // Spec 2.2: L = 0 -> 0.5, 1022 -> 1.522, 8191 -> 8.691.
        for (l, want) in [(0u16, 0.5), (1022, 1.522), (1023, 1.523), (8191, 8.691)] {
            let bytes = encode(&[Lc1Words { function: 0, value: l, multiplier: 147 }], &[]);
            let p = Decoder::new().push(&bytes);
            let s = p[0].sensors[0];
            assert!((s.lambda.unwrap() - want).abs() < 1e-9, "{l}");
            assert_eq!(s.stoich, 14.7);
        }
    }

    #[test]
    fn afr_formula_matches_the_spec() {
        // AFR = (L + 500) * AF / 10000.
        let w = Lc1Words { function: 0, value: 500, multiplier: 147 };
        let s = Decoder::new().push(&encode(&[w], &[]))[0].sensors[0];
        assert!((s.afr().unwrap() - (500.0 + 500.0) * 147.0 / 10000.0).abs() < 1e-9);
    }

    #[test]
    fn header_bits_follow_the_spec() {
        let bytes = encode(&[Lc1Words::lambda(1.0)], &[]);
        // 1 R 1 D/S HF X 1 B7 | 1 B6..B0 with D/S = 1, length 2.
        assert_eq!(bytes[..2], [0xB2, 0x82]);
        // LC-1 word 0: 0 1 0 F2 F1 F0 1 AF7 | 0 AF6..AF0; 147 = 0x93.
        assert_eq!(bytes[2..4], [0x43, 0x13]);
        assert!(bytes[2..].iter().all(|b| b & 0x80 == 0));
    }

    #[test]
    fn status_words() {
        let cases = [
            (1, 209, Status::O2Level(20.9)),
            (2, 0, Status::FreeAirCalibration),
            (3, 0, Status::NeedsFreeAirCalibration),
            (4, 455, Status::WarmingUp(45.5)),
            (5, 12, Status::HeaterCalibration(12)),
            (6, 8, Status::Error(8)),
        ];
        for (f, v, want) in cases {
            let w = Lc1Words { function: f, value: v, multiplier: 147 };
            let s = Decoder::new().push(&encode(&[w], &[]))[0].sensors[0];
            assert_eq!(s.status, want);
            assert_eq!(s.lambda, None);
            assert_eq!(Status::parse(&want.text()), Some(want));
        }
        assert_eq!(Status::parse("ok"), Some(Status::Normal));
    }

    #[test]
    fn resyncs_through_garbage_and_split_reads() {
        let mut stream = vec![0x13, 0x00, 0x7F, 0x82];
        for i in 0..5 {
            stream.extend(encode(&[Lc1Words::lambda(0.9 + 0.05 * f64::from(i))], &[300]));
        }
        let mut d = Decoder::new();
        let mut got = Vec::new();
        for chunk in stream.chunks(3) {
            got.extend(d.push(chunk));
        }
        assert_eq!(got.len(), 5);
        assert!((got[4].sensors[0].lambda.unwrap() - 1.1).abs() < 1e-9);
        assert_eq!(got[0].aux, vec![300]);
        assert!(d.skipped >= 4);
    }

    #[test]
    fn first_device_sets_the_fuel_for_all() {
        let a = Lc1Words { function: 0, value: 500, multiplier: 147 };
        let b = Lc1Words { function: 0, value: 400, multiplier: 64 };
        let s = &Decoder::new().push(&encode(&[a, b], &[]))[0].sensors;
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].stoich, 14.7);
        assert!((s[1].lambda.unwrap() - 0.9).abs() < 1e-9);
    }

    #[test]
    fn lm1_sub_packet_and_command_responses() {
        // Header (length 8) + LM-1: function word with bit 15 set, lambda,
        // battery, 5 aux.
        let header: u16 = HEADER_MASK | 0x1000 | 8;
        let mut b = header.to_be_bytes().to_vec();
        let w0: u16 = 0x8000 | 0x13; // AF6..0 = 0x13, AF7 (bit 8) set -> 147
        let w0 = w0 | 0x0100;
        b.extend(w0.to_be_bytes());
        b.extend(0x0374u16.to_be_bytes()); // L = 500 split 6+7 bits -> lambda 1.0
        for _ in 0..6 {
            b.extend(0u16.to_be_bytes());
        }
        // A command response (D/S = 0) of 1 word is skipped.
        let resp: u16 = HEADER_MASK | 1;
        b.extend(resp.to_be_bytes());
        b.extend(0x0042u16.to_be_bytes());
        let p = Decoder::new().push(&b);
        assert_eq!(p.len(), 1);
        let s = p[0].sensors[0];
        assert_eq!(s.stoich, 14.7);
        assert!((s.lambda.unwrap() - (500.0 + 500.0) / 1000.0).abs() < 1e-9);
        assert_eq!(p[0].aux.len(), 5);
    }
}
