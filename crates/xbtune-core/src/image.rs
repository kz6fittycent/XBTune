// SPDX-License-Identifier: GPL-3.0-or-later
//! EEPROM images and their file formats.
//!
//! **`.xbt` (XBTune image)**: a TOML file with metadata and the bytes as an
//! offset-prefixed hex dump, plus a CRC-32 of the bytes. Human-readable,
//! diffable, and checked on load. Specified in `docs/FILE_FORMATS.md`.
//!
//! **`.xpr` (EcmDroid raw image)**: just the bytes. EcmDroid writes the whole
//! `xsize` buffer (page 0 last) and loads either `size` or `xsize` bytes,
//! picking the ECM type by length (`EEPROMFragment.java`, `EEPROM.size2id()`,
//! commit 2f17d29f). Its file names start with the ECM ID:
//! `BUEIB_20261014-194005.xpr`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use xbtune_defs::{Catalog, EcmDef, ImageView};

/// Format identifier in `.xbt` files.
pub const XBT_FORMAT: &str = "xbtune-eeprom";
/// Current `.xbt` format version.
pub const XBT_VERSION: u32 = 1;

/// A full EEPROM image of one ECM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Five-character ECM ID, e.g. `BUEIB`.
    pub ecm_id: String,
    /// Full version string read from the ECM, if known.
    pub ecm_version: Option<String>,
    /// Always `xsize` bytes: pages 1..N, then page 0.
    pub bytes: Vec<u8>,
    /// True if the page 0 bytes are real (read from the ECM or a file that
    /// had them), false if they are zero padding.
    pub page_zero: bool,
    pub meta: Meta,
}

/// Free-form metadata kept with an image.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// When the image was made (RFC 3339 UTC).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created: String,
    /// Where it came from, e.g. "read from ECM over Bluetooth 00:11:22:33:44:55".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub vin: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("not an XBTune image: {0}")]
    Parse(String),
    #[error("unknown ECM ID '{0}'")]
    UnknownEcm(String),
    #[error("{id}: image has {len} bytes, expected {size} or {xsize}")]
    WrongSize { id: String, len: usize, size: usize, xsize: usize },
    #[error(
        "checksum mismatch: file says {stored:08x}, bytes give {actual:08x}. The file is damaged or was edited"
    )]
    Crc { stored: u32, actual: u32 },
    #[error(
        "cannot tell which ECM this {len}-byte image is for (candidates: {candidates}). Give the ID explicitly"
    )]
    Ambiguous { len: usize, candidates: String },
}

impl Image {
    /// Wrap raw bytes for `ecm`. Accepts `size` (no page 0) or `xsize` bytes.
    pub fn from_raw(ecm: &EcmDef, raw: &[u8]) -> Result<Self, ImageError> {
        let page_zero = if raw.len() == ecm.xsize {
            ecm.xsize != ecm.size
        } else if raw.len() == ecm.size {
            false
        } else {
            return Err(ImageError::WrongSize {
                id: ecm.id.clone(),
                len: raw.len(),
                size: ecm.size,
                xsize: ecm.xsize,
            });
        };
        let mut bytes = raw.to_vec();
        bytes.resize(ecm.xsize, 0);
        Ok(Self { ecm_id: ecm.id.clone(), ecm_version: None, bytes, page_zero, meta: Meta::default() })
    }

    pub fn view(&self) -> ImageView<'_> {
        ImageView { bytes: &self.bytes, page_zero: self.page_zero }
    }

    /// Bytes as EcmDroid would save them: all `xsize` bytes if page 0 is
    /// real, otherwise only `size` bytes so padding is never taken as data.
    pub fn to_xpr(&self, ecm: &EcmDef) -> Vec<u8> {
        if self.page_zero { self.bytes.clone() } else { self.bytes[..ecm.size].to_vec() }
    }

    /// Read an EcmDroid `.xpr`. The ECM ID comes from `id`, else from the
    /// file name prefix, else from the length if that is unambiguous.
    pub fn read_xpr(path: &Path, id: Option<&str>, catalog: &Catalog) -> Result<Self, ImageError> {
        let raw = std::fs::read(path)?;
        let from_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.get(..5))
            .filter(|p| catalog.lookup(p).is_some());
        let ecm = match id.or(from_name) {
            Some(id) => catalog.lookup(id).ok_or_else(|| ImageError::UnknownEcm(id.to_string()))?,
            None => {
                let c = catalog.ids_for_image_len(raw.len());
                // IDs sharing one definition and size are interchangeable here.
                let mut defs: Vec<&str> = c.iter().map(|d| d.definition.as_str()).collect();
                defs.dedup();
                match (c.first(), defs.len()) {
                    (Some(first), 1) => *first,
                    _ => {
                        return Err(ImageError::Ambiguous {
                            len: raw.len(),
                            candidates: c.iter().map(|d| d.id.as_str()).collect::<Vec<_>>().join(", "),
                        });
                    }
                }
            }
        };
        let mut img = Self::from_raw(ecm, &raw)?;
        img.meta.source = format!("imported from {}", path.display());
        Ok(img)
    }

    /// Serialize as `.xbt`.
    pub fn to_xbt(&self) -> String {
        let mut s = String::new();
        s.push_str("# XBTune EEPROM image. Format: docs/FILE_FORMATS.md\n");
        s.push_str("# Edit the metadata freely; editing the hex breaks the CRC on purpose.\n");
        let head = Header {
            format: XBT_FORMAT.into(),
            format_version: XBT_VERSION,
            ecm_id: self.ecm_id.clone(),
            ecm_version: self.ecm_version.clone(),
            length: self.bytes.len(),
            page_zero: self.page_zero,
            crc32: format!("{:08x}", crc32(&self.bytes)),
            meta: Meta::default(),
            data: String::new(),
        };
        // Top-level keys first, then `data`, then the [meta] table: in TOML,
        // keys after a table header belong to that table.
        let head_toml = toml::to_string(&head).expect("header serializes");
        for line in head_toml.lines() {
            if line.starts_with("data =") || line.starts_with("[meta]") {
                continue;
            }
            s.push_str(line);
            s.push('\n');
        }
        s.push_str("data = \"\"\"\n");
        for (i, row) in self.bytes.chunks(16).enumerate() {
            s.push_str(&format!("{:04x}:", i * 16));
            for b in row {
                s.push_str(&format!(" {b:02x}"));
            }
            s.push('\n');
        }
        s.push_str("\"\"\"\n\n[meta]\n");
        s.push_str(&toml::to_string(&self.meta).expect("meta serializes"));
        s
    }

    /// Parse `.xbt` text and check it against the catalog.
    pub fn from_xbt(text: &str, catalog: &Catalog) -> Result<Self, ImageError> {
        let head: Header = toml::from_str(text).map_err(|e| ImageError::Parse(e.to_string()))?;
        if head.format != XBT_FORMAT {
            return Err(ImageError::Parse(format!("format is '{}', expected '{XBT_FORMAT}'", head.format)));
        }
        if head.format_version > XBT_VERSION {
            return Err(ImageError::Parse(format!(
                "format version {} is newer than this XBTune understands ({XBT_VERSION})",
                head.format_version
            )));
        }
        let mut bytes = Vec::with_capacity(head.length);
        for (n, line) in head.data.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let hex = line.split_once(':').map_or(line, |(_, rest)| rest);
            for tok in hex.split_whitespace() {
                let b = u8::from_str_radix(tok, 16)
                    .map_err(|_| ImageError::Parse(format!("bad hex '{tok}' on data line {}", n + 1)))?;
                bytes.push(b);
            }
        }
        if bytes.len() != head.length {
            return Err(ImageError::Parse(format!("length says {}, data has {}", head.length, bytes.len())));
        }
        let stored =
            u32::from_str_radix(&head.crc32, 16).map_err(|_| ImageError::Parse("bad crc32".into()))?;
        let actual = crc32(&bytes);
        if stored != actual {
            return Err(ImageError::Crc { stored, actual });
        }
        let ecm = catalog.lookup(&head.ecm_id).ok_or_else(|| ImageError::UnknownEcm(head.ecm_id.clone()))?;
        if bytes.len() != ecm.xsize {
            return Err(ImageError::WrongSize {
                id: ecm.id.clone(),
                len: bytes.len(),
                size: ecm.size,
                xsize: ecm.xsize,
            });
        }
        Ok(Self {
            ecm_id: ecm.id.clone(),
            ecm_version: head.ecm_version,
            bytes,
            page_zero: head.page_zero && ecm.xsize != ecm.size,
            meta: head.meta,
        })
    }

    /// Load `.xbt` or `.xpr` by extension (`id` only applies to `.xpr`).
    pub fn load(path: &Path, id: Option<&str>, catalog: &Catalog) -> Result<Self, ImageError> {
        if is_xpr(path) {
            Self::read_xpr(path, id, catalog)
        } else {
            Self::from_xbt(&std::fs::read_to_string(path)?, catalog)
        }
    }

    /// Save as `.xbt` or `.xpr` by extension. Writes to a temporary file and
    /// renames, so a crash never leaves a half-written image.
    pub fn save(&self, path: &Path, catalog: &Catalog) -> Result<(), ImageError> {
        let data = if is_xpr(path) {
            let ecm =
                catalog.lookup(&self.ecm_id).ok_or_else(|| ImageError::UnknownEcm(self.ecm_id.clone()))?;
            self.to_xpr(ecm)
        } else {
            self.to_xbt().into_bytes()
        };
        if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp-xbtune");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn is_xpr(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xpr"))
}

#[derive(Serialize, Deserialize)]
struct Header {
    format: String,
    format_version: u32,
    ecm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ecm_version: Option<String>,
    length: usize,
    page_zero: bool,
    crc32: String,
    #[serde(default, skip_serializing_if = "meta_is_empty")]
    meta: Meta,
    #[serde(default)]
    data: String,
}

fn meta_is_empty(m: &Meta) -> bool {
    *m == Meta::default()
}

/// CRC-32 (IEEE 802.3, as zlib/PNG).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Byte ranges where two images differ, as (start, end) pairs.
pub fn diff_ranges(a: &[u8], b: &[u8]) -> Vec<(usize, usize)> {
    let n = a.len().max(b.len());
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if a.get(i) != b.get(i) {
            let start = i;
            while i < n && a.get(i) != b.get(i) {
                i += 1;
            }
            out.push((start, i));
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUEIB: &[u8] = include_bytes!("../../../tests/fixtures/BUEIB.eeprom");

    fn catalog() -> Catalog {
        Catalog::builtin().unwrap()
    }

    #[test]
    fn crc32_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn raw_sizes() {
        let c = catalog();
        let ecm = c.lookup("BUEIB").unwrap();
        let img = Image::from_raw(ecm, BUEIB).unwrap();
        assert!(!img.page_zero);
        assert_eq!(img.bytes.len(), 1210);
        assert_eq!(img.to_xpr(ecm), BUEIB);
        let mut full = BUEIB.to_vec();
        full.extend_from_slice(&[1, 2, 3, 4]);
        let img = Image::from_raw(ecm, &full).unwrap();
        assert!(img.page_zero);
        assert_eq!(img.to_xpr(ecm), full);
        assert!(matches!(Image::from_raw(ecm, &full[..1000]), Err(ImageError::WrongSize { .. })));
    }

    #[test]
    fn xbt_roundtrip_and_crc() {
        let c = catalog();
        let mut img = Image::from_raw(c.lookup("BUEIB").unwrap(), BUEIB).unwrap();
        img.ecm_version = Some("BUEIB310 12-11-03".into());
        img.meta.notes = "stock map, \"quoted\"\nsecond line".into();
        img.meta.vin = "4MZAX12345Y123456".into();
        let text = img.to_xbt();
        assert!(text.contains("0000: "));
        let back = Image::from_xbt(&text, &c).unwrap();
        assert_eq!(back, img);
        // Change one data byte: the CRC must catch it.
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        let i = lines.iter().position(|l| l.starts_with("0010:")).unwrap();
        let new = if &lines[i][6..8] == "00" { "01" } else { "00" };
        lines[i].replace_range(6..8, new);
        assert!(matches!(Image::from_xbt(&lines.join("\n"), &c), Err(ImageError::Crc { .. })));
        // Drop a byte: the length check must catch it.
        lines[i] = text.lines().nth(i).unwrap()[..lines[i].len() - 3].to_string();
        assert!(matches!(Image::from_xbt(&lines.join("\n"), &c), Err(ImageError::Parse(_))));
    }

    #[test]
    fn xpr_id_detection() {
        let c = catalog();
        let dir = std::env::temp_dir().join(format!("xbtune-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let named = dir.join("BUEIB_20261014-120000.xpr");
        std::fs::write(&named, BUEIB).unwrap();
        assert_eq!(Image::read_xpr(&named, None, &c).unwrap().ecm_id, "BUEIB");
        // 1206 bytes fits BUEIB, BUEIC and B2RIB, which share one definition.
        let anon = dir.join("dump.xpr");
        std::fs::write(&anon, BUEIB).unwrap();
        assert_eq!(Image::read_xpr(&anon, None, &c).unwrap().bytes.len(), 1210);
        let odd = dir.join("odd.xpr");
        std::fs::write(&odd, [0u8; 7]).unwrap();
        assert!(matches!(Image::read_xpr(&odd, None, &c), Err(ImageError::Ambiguous { .. })));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_and_load_both_formats() {
        let c = catalog();
        let img = Image::from_raw(c.lookup("BUEIB").unwrap(), BUEIB).unwrap();
        let dir = std::env::temp_dir().join(format!("xbtune-save-{}", std::process::id()));
        for name in ["a.xbt", "BUEIB_a.xpr"] {
            let p = dir.join(name);
            img.save(&p, &c).unwrap();
            let back = Image::load(&p, None, &c).unwrap();
            assert_eq!(back.bytes, img.bytes, "{name}");
            assert_eq!(back.page_zero, img.page_zero, "{name}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn diff() {
        assert_eq!(diff_ranges(&[1, 2, 3, 4], &[1, 9, 9, 4]), vec![(1, 3)]);
        assert_eq!(diff_ranges(&[1, 2], &[1, 2, 3]), vec![(2, 3)]);
        assert!(diff_ranges(&[1], &[1]).is_empty());
    }
}
