// SPDX-License-Identifier: GPL-3.0-or-later
//! Pairing and frame encryption (docs/REMOTE.md 4.2-4.5).
//!
//! SPAKE2 (Ed25519 group) turns the 6 secret digits into a shared key
//! without letting the relay (or anyone) check guesses offline; HKDF-SHA256
//! derives one key per direction plus a resume secret; ChaCha20-Poly1305
//! seals every frame with a per-direction counter as the nonce.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

/// Frame format version (first byte of every binary frame).
pub const VERSION: u8 = 1;
pub const KIND_PAKE: u8 = 0;
pub const KIND_DATA: u8 = 1;
/// Largest frame either side accepts.
pub const MAX_FRAME: usize = 1 << 20;
const SALT: &[u8] = b"xbtune-remote/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Guest,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Host => "host",
            Role::Guest => "guest",
        }
    }

    pub fn other(self) -> Role {
        match self {
            Role::Host => Role::Guest,
            Role::Guest => Role::Host,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("malformed frame")]
    BadFrame,
    #[error("frame too large ({0} bytes)")]
    TooLarge(usize),
    #[error("pairing failed")]
    Pake,
    #[error("frame didn't decrypt (wrong code or tampered)")]
    Decrypt,
    #[error("frame out of order (expected {expected}, got {got})")]
    Counter { expected: u64, got: u64 },
}

/// The password for an epoch: the 6 digits for epoch 0, then the resume
/// secret of the epoch before (4.4).
#[derive(Clone)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn digits(d: &str) -> Self {
        Secret(d.as_bytes().to_vec())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// A pairing in progress for one epoch.
pub struct Pairing {
    spake: Spake2<Ed25519Group>,
    role: Role,
    room: String,
    pub epoch: u32,
}

/// `[version][kind=pake][epoch u32 BE][spake2 message]`.
pub fn pake_frame(epoch: u32, msg: &[u8]) -> Vec<u8> {
    let mut f = vec![VERSION, KIND_PAKE];
    f.extend_from_slice(&epoch.to_be_bytes());
    f.extend_from_slice(msg);
    f
}

/// The epoch and SPAKE2 message of a pake frame.
pub fn parse_pake(frame: &[u8]) -> Result<(u32, &[u8]), CryptoError> {
    if frame.len() < 6 + 1 || frame.len() > 256 || frame[0] != VERSION || frame[1] != KIND_PAKE {
        return Err(CryptoError::BadFrame);
    }
    let epoch = u32::from_be_bytes(frame[2..6].try_into().map_err(|_| CryptoError::BadFrame)?);
    Ok((epoch, &frame[6..]))
}

/// What kind of frame this is, if it's one of ours at all.
pub fn frame_kind(frame: &[u8]) -> Option<u8> {
    (frame.len() >= 2 && frame[0] == VERSION).then(|| frame[1])
}

fn identity(role: Role, room: &str) -> Identity {
    Identity::new(format!("xbtune-remote/1 {} room={room}", role.name()).as_bytes())
}

impl Pairing {
    /// Start pairing; returns the frame to send to the other side.
    pub fn start(role: Role, room: &str, epoch: u32, secret: &Secret) -> (Self, Vec<u8>) {
        let pw = Password::new(&secret.0);
        let (a, b) = (identity(Role::Host, room), identity(Role::Guest, room));
        let (spake, msg) = match role {
            Role::Host => Spake2::<Ed25519Group>::start_a(&pw, &a, &b),
            Role::Guest => Spake2::<Ed25519Group>::start_b(&pw, &a, &b),
        };
        (Self { spake, role, room: room.to_string(), epoch }, pake_frame(epoch, &msg))
    }

    /// Finish with the other side's SPAKE2 message (from [`parse_pake`]).
    /// Wrong digits are NOT detected here: the keys just differ, and the
    /// first sealed frame fails to open (4.3).
    pub fn finish(self, their_msg: &[u8]) -> Result<Keys, CryptoError> {
        let k = self.spake.finish(their_msg).map_err(|_| CryptoError::Pake)?;
        Ok(Keys::derive(&k, self.role, &self.room, self.epoch))
    }
}

/// Session keys for one epoch.
pub struct Keys {
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
    send_dir: u8,
    recv_dir: u8,
    room: String,
    pub epoch: u32,
    send_ctr: u64,
    recv_ctr: u64,
    resume: [u8; 32],
}

impl Keys {
    fn derive(k: &[u8], role: Role, room: &str, epoch: u32) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(SALT), k);
        let expand = |label: &str| {
            let mut info = label.as_bytes().to_vec();
            info.extend_from_slice(room.as_bytes());
            info.extend_from_slice(&epoch.to_be_bytes());
            let mut out = [0u8; 32];
            hk.expand(&info, &mut out).expect("32 bytes is a valid HKDF-SHA256 length");
            out
        };
        let (hg, gh, rs) = (expand("host->guest"), expand("guest->host"), expand("resume"));
        let cipher = |k: &[u8; 32]| ChaCha20Poly1305::new(Key::from_slice(k));
        let (send, recv, send_dir, recv_dir) = match role {
            Role::Host => (cipher(&hg), cipher(&gh), 0, 1),
            Role::Guest => (cipher(&gh), cipher(&hg), 1, 0),
        };
        Self {
            send,
            recv,
            send_dir,
            recv_dir,
            room: room.to_string(),
            epoch,
            send_ctr: 0,
            recv_ctr: 0,
            resume: rs,
        }
    }

    /// The password for the next epoch.
    pub fn resume_secret(&self) -> Secret {
        Secret(self.resume.to_vec())
    }

    fn aad(&self, dir: u8) -> Vec<u8> {
        let mut aad = vec![VERSION, KIND_DATA, dir];
        aad.extend_from_slice(self.room.as_bytes());
        aad
    }

    fn nonce(ctr: u64) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&ctr.to_be_bytes());
        n
    }

    /// `[version][kind=data][counter u64 BE][ciphertext + tag]`.
    pub fn seal(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let ctr = self.send_ctr;
        self.send_ctr += 1;
        let aad = self.aad(self.send_dir);
        let ct = self
            .send
            .encrypt(Nonce::from_slice(&Self::nonce(ctr)), Payload { msg: plaintext, aad: &aad })
            .expect("ChaCha20-Poly1305 encryption doesn't fail for in-memory buffers");
        let mut f = vec![VERSION, KIND_DATA];
        f.extend_from_slice(&ctr.to_be_bytes());
        f.extend_from_slice(&ct);
        f
    }

    /// Open the next frame from the other side. Any error means the epoch
    /// is over (4.5): the caller drops the keys.
    pub fn open(&mut self, frame: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if frame.len() > MAX_FRAME {
            return Err(CryptoError::TooLarge(frame.len()));
        }
        if frame.len() < 10 + 16 || frame[0] != VERSION || frame[1] != KIND_DATA {
            return Err(CryptoError::BadFrame);
        }
        let ctr = u64::from_be_bytes(frame[2..10].try_into().map_err(|_| CryptoError::BadFrame)?);
        if ctr != self.recv_ctr {
            return Err(CryptoError::Counter { expected: self.recv_ctr, got: ctr });
        }
        let aad = self.aad(self.recv_dir);
        let pt = self
            .recv
            .decrypt(Nonce::from_slice(&Self::nonce(ctr)), Payload { msg: &frame[10..], aad: &aad })
            .map_err(|_| CryptoError::Decrypt)?;
        self.recv_ctr += 1;
        Ok(pt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(host_digits: &str, guest_digits: &str, room_h: &str, room_g: &str) -> (Keys, Keys) {
        let (h, hf) = Pairing::start(Role::Host, room_h, 0, &Secret::digits(host_digits));
        let (g, gf) = Pairing::start(Role::Guest, room_g, 0, &Secret::digits(guest_digits));
        let (_, gm) = parse_pake(&gf).unwrap();
        let (_, hm) = parse_pake(&hf).unwrap();
        (h.finish(gm).unwrap(), g.finish(hm).unwrap())
    }

    #[test]
    fn same_digits_talk_both_ways() {
        let (mut h, mut g) = pair("381926", "381926", "47", "47");
        for i in 0..5 {
            let f = h.seal(format!("hello {i}").as_bytes());
            assert_eq!(g.open(&f).unwrap(), format!("hello {i}").as_bytes());
            let f = g.seal(b"back");
            assert_eq!(h.open(&f).unwrap(), b"back");
        }
        assert_eq!(h.resume_secret().0, g.resume_secret().0);
    }

    #[test]
    fn wrong_digits_or_room_fail_on_first_frame() {
        let (mut h, mut g) = pair("381926", "381927", "47", "47");
        assert_eq!(g.open(&h.seal(b"x")), Err(CryptoError::Decrypt));
        let (mut h, mut g) = pair("381926", "381926", "47", "48");
        assert_eq!(g.open(&h.seal(b"x")), Err(CryptoError::Decrypt));
    }

    #[test]
    fn tampered_replayed_reordered_and_reflected_frames_fail() {
        let (mut h, mut g) = pair("000001", "000001", "5", "5");
        let f0 = h.seal(b"zero");
        let f1 = h.seal(b"one");
        // Reordered.
        assert!(matches!(g.open(&f1), Err(CryptoError::Counter { expected: 0, got: 1 })));
        assert_eq!(g.open(&f0).unwrap(), b"zero");
        // Replayed.
        assert!(matches!(g.open(&f0), Err(CryptoError::Counter { .. })));
        // Tampered.
        let mut bad = f1.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert_eq!(g.open(&bad), Err(CryptoError::Decrypt));
        assert_eq!(g.open(&f1).unwrap(), b"one");
        // Truncated / junk.
        assert_eq!(g.open(&f1[..12]), Err(CryptoError::BadFrame));
        assert_eq!(g.open(&[0u8; 40]), Err(CryptoError::BadFrame));
        // A host frame sent back to the host (reflection) doesn't open,
        // even with matching counters.
        let (mut h, _) = pair("000001", "000001", "5", "5");
        let first = h.seal(b"zero");
        assert_eq!(h.open(&first), Err(CryptoError::Decrypt));
    }

    #[test]
    fn epochs_and_resume_give_new_keys() {
        let (h0, g0) = pair("123456", "123456", "9", "9");
        let (h1, h1f) = Pairing::start(Role::Host, "9", 1, &h0.resume_secret());
        let (g1, g1f) = Pairing::start(Role::Guest, "9", 1, &g0.resume_secret());
        let (e, gm) = parse_pake(&g1f).unwrap();
        assert_eq!(e, 1);
        let (_, hm) = parse_pake(&h1f).unwrap();
        let (mut h1, mut g1) = (h1.finish(gm).unwrap(), g1.finish(hm).unwrap());
        // Keys differ from epoch 0's: an epoch-0 frame (same counter 0)
        // doesn't open in epoch 1.
        let mut h0 = h0;
        let old = h0.seal(b"old");
        assert_eq!(g1.open(&old), Err(CryptoError::Decrypt));
        assert_eq!(g1.open(&h1.seal(b"resumed")).unwrap(), b"resumed");
    }

    #[test]
    fn pake_frames_parse_strictly() {
        assert!(parse_pake(&[VERSION, KIND_PAKE, 0, 0, 0, 1]).is_err());
        assert!(parse_pake(&[2, KIND_PAKE, 0, 0, 0, 1, 7]).is_err());
        assert!(parse_pake(&vec![VERSION; 300]).is_err());
        assert_eq!(parse_pake(&pake_frame(7, &[1, 2])).unwrap(), (7, &[1u8, 2][..]));
    }
}
