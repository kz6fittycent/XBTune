// SPDX-License-Identifier: GPL-3.0-or-later
//! The code the host reads out: `47-381926` = room number (which session,
//! the relay knows it) + 6 secret digits (never leave the two apps).
//! docs/REMOTE.md section 1.1.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub room: String,
    pub digits: String,
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.room, self.digits)
    }
}

/// 6 random digits from the system's secure random source (via rustls's
/// ring provider), without modulo bias.
pub fn new_digits() -> String {
    let rng = rustls::crypto::ring::default_provider().secure_random;
    loop {
        let mut b = [0u8; 4];
        rng.fill(&mut b).expect("the system random source works");
        let n = u32::from_be_bytes(b);
        // 4,294,000,000 is the largest multiple of 1,000,000 below 2^32.
        if n < 4_294_000_000 {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

/// A valid room number: 1-3 digits, no leading zero.
pub fn valid_room(room: &str) -> bool {
    (1..=3).contains(&room.len()) && room.bytes().all(|b| b.is_ascii_digit()) && !room.starts_with('0')
}

/// Read what the tuner typed: `47-381926`, `47 381 926`, `47381926`.
pub fn parse(text: &str) -> Result<Code, String> {
    let digits: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let (room, secret) = match digits.split_once(['-', '–', '.', '/']) {
        Some((r, s)) => (r.to_string(), s.replace(['-', '–', '.', '/'], "")),
        None if digits.len() > 6 => {
            let (r, s) = digits.split_at(digits.len() - 6);
            (r.to_string(), s.to_string())
        }
        None => return Err("Type the whole code, like 47-381926.".into()),
    };
    if !valid_room(&room) {
        return Err(format!("'{room}' isn't a room number (1 to 3 digits before the dash)."));
    }
    if secret.len() != 6 || !secret.bytes().all(|b| b.is_ascii_digit()) {
        return Err("The part after the dash must be 6 digits.".into());
    }
    Ok(Code { room, digits: secret })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_people_type() {
        let want = Code { room: "47".into(), digits: "381926".into() };
        for t in ["47-381926", " 47 - 381 926 ", "47381926", "47–381926", "47-381-926"] {
            assert_eq!(parse(t), Ok(want.clone()), "{t}");
        }
        assert_eq!(parse("7-000001").unwrap().to_string(), "7-000001");
        for bad in ["", "381926", "47-38192", "047-381926", "1000-381926", "ab-381926", "47-38192x"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn digits_are_six_and_vary() {
        let a: Vec<String> = (0..20).map(|_| new_digits()).collect();
        assert!(a.iter().all(|d| d.len() == 6 && d.bytes().all(|b| b.is_ascii_digit())));
        assert!(a.iter().collect::<std::collections::HashSet<_>>().len() > 15);
    }
}
