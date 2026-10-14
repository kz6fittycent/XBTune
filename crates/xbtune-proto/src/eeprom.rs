// SPDX-License-Identifier: GPL-3.0-or-later
//! EEPROM page model and read/write chunk plans.
//!
//! Ported from EcmDroid `app/src/main/java/org/ecmdroid/EEPROM.java`
//! (`EEPROM.get()`, `EEPROM.Page`) and `ECM.java` (`readEEPromPage()`,
//! `writeEEPromPage()`), commit 2f17d29f5b08f67fb357ee8f2ec21e7ae930343e.
//! Copyright (C) 2012 by Michel Marti. Rust port Copyright (C) 2026 XBTune
//! contributors. Licensed GPL-3.0-or-later.
//!
//! The EEPROM image is laid out as pages 1..N back to back, starting at
//! offset 0, followed by page 0 (if the ECM has one) at the very end. That is
//! why EcmDroid has two sizes per ECM: `size` (pages 1..N) and `xsize`
//! (including page 0). Page 0 is special on the wire: it is read one byte at
//! a time, at in-page offsets that end at 0xFF.

use crate::pdu::Request;

/// Largest number of bytes EcmDroid moves in one GET or SET.
pub const MAX_CHUNK: usize = 16;

/// Page 0 always ends at in-page offset 0xFF (`ECM.PAGE_ZERO_OFFSET`).
pub const PAGE_ZERO_END: usize = 0xFF;

/// One EEPROM page and where it lives in the flat image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub nr: u8,
    pub len: usize,
    /// Offset of the page's first byte in the flat image.
    pub start: usize,
}

/// Page layout of one ECM type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Image size including page 0 (`xsize`).
    pub xsize: usize,
    /// Pages in page-number order.
    pub pages: Vec<Page>,
}

/// Invalid layout data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("pages 1..N cover {covered} bytes, page 0 has {zero} bytes, xsize is {xsize}")]
    SizeMismatch { covered: usize, zero: usize, xsize: usize },
    #[error("page {0} listed twice")]
    DuplicatePage(u8),
    #[error("page {nr} has invalid length {len}")]
    BadPageLength { nr: u8, len: usize },
    #[error("chunk outside page {nr}: offset {offset} + {len} > {page_len}")]
    OutOfPage { nr: u8, offset: usize, len: usize, page_len: usize },
    #[error("no page {0}")]
    NoSuchPage(u8),
}

impl Layout {
    /// Build a layout from `(page number, page length)` pairs, as stored in
    /// ecmdroid.db's `pages` table. Mirrors `EEPROM.get()`: pages other than
    /// 0 are placed back to back in page-number order, page 0 at the end.
    pub fn new(xsize: usize, page_sizes: &[(u8, usize)]) -> Result<Self, LayoutError> {
        let mut sorted = page_sizes.to_vec();
        sorted.sort_by_key(|&(nr, _)| nr);
        for w in sorted.windows(2) {
            if w[0].0 == w[1].0 {
                return Err(LayoutError::DuplicatePage(w[0].0));
            }
        }
        let mut pages = Vec::with_capacity(sorted.len());
        let mut pc = 0;
        let mut zero = 0;
        for &(nr, len) in &sorted {
            let max = if nr == 0 { PAGE_ZERO_END + 1 } else { 256 };
            if len == 0 || len > max {
                return Err(LayoutError::BadPageLength { nr, len });
            }
            if nr == 0 {
                zero = len;
                pages.push(Page { nr, len, start: xsize.wrapping_sub(len) });
            } else {
                pages.push(Page { nr, len, start: pc });
                pc += len;
            }
        }
        if pc + zero != xsize {
            return Err(LayoutError::SizeMismatch { covered: pc, zero, xsize });
        }
        Ok(Self { xsize, pages })
    }

    /// Image size without page 0 (`size` in ecmdroid.db).
    pub fn size_without_page_zero(&self) -> usize {
        self.xsize - self.page(0).map_or(0, |p| p.len)
    }

    pub fn page(&self, nr: u8) -> Option<&Page> {
        self.pages.iter().find(|p| p.nr == nr)
    }

    /// Pages that contain any byte of `range` (flat image offsets).
    pub fn pages_touching(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = &Page> {
        self.pages.iter().filter(move |p| range.start < p.start + p.len && p.start < range.end)
    }

    /// Requests that read page `nr` completely, in order.
    pub fn read_plan(&self, nr: u8) -> Result<Vec<Chunk>, LayoutError> {
        let page = *self.page(nr).ok_or(LayoutError::NoSuchPage(nr))?;
        Ok(chunks(page, 0, page.len))
    }

    /// Requests that write bytes `offset..offset+len` of page `nr`.
    ///
    /// Page 0 is refused: EcmDroid only writes selected variables there
    /// (`PAGE_ZERO_VARS_TO_WRITE`), which needs definitions. That is handled
    /// by the burn code in a later phase, not here.
    pub fn write_plan(&self, nr: u8, offset: usize, len: usize) -> Result<Vec<Chunk>, LayoutError> {
        let page = *self.page(nr).ok_or(LayoutError::NoSuchPage(nr))?;
        if nr == 0 {
            return Err(LayoutError::NoSuchPage(0));
        }
        if offset + len > page.len {
            return Err(LayoutError::OutOfPage { nr, offset, len, page_len: page.len });
        }
        Ok(chunks(page, offset, len))
    }
}

/// One wire transfer: `len` bytes at in-page `wire_offset` of page `page`,
/// which map to `image_offset..image_offset+len` of the flat image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub page: u8,
    pub wire_offset: u8,
    pub len: u8,
    pub image_offset: usize,
}

impl Chunk {
    pub fn get_request(&self) -> Request {
        Request::Get { page: self.page, offset: self.wire_offset, len: self.len }
    }

    pub fn set_request(&self, image: &[u8]) -> Request {
        let data = image[self.image_offset..self.image_offset + self.len as usize].to_vec();
        Request::Set { page: self.page, offset: self.wire_offset, data }
    }

    pub fn image_range(&self) -> std::ops::Range<usize> {
        self.image_offset..self.image_offset + self.len as usize
    }
}

fn chunks(page: Page, from: usize, len: usize) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut i = from;
    while i < from + len {
        let (wire_offset, n) = if page.nr == 0 {
            // ECM.readEEPromPage: offset = PAGE_ZERO_OFFSET - page.length() + i + 1, one byte.
            (PAGE_ZERO_END - page.len + i + 1, 1)
        } else {
            (i, (from + len - i).min(MAX_CHUNK))
        };
        out.push(Chunk {
            page: page.nr,
            wire_offset: wire_offset as u8,
            len: n as u8,
            image_offset: page.start + i,
        });
        i += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BUEIB (category 8) from ecmdroid.db `pages`/`eeprom` tables.
    fn bueib() -> Layout {
        Layout::new(1210, &[(0, 4), (1, 256), (2, 256), (3, 158), (4, 256), (5, 256), (6, 24)]).unwrap()
    }

    #[test]
    fn bueib_layout_matches_ecmdroid_test() {
        // EcmDroid TestEEPROM.testVersion asserts these starts. Note it
        // asserts page 0 starts at 1206 for xsize 1210 and page 0 length 4.
        let l = bueib();
        assert_eq!(l.pages.len(), 7);
        assert_eq!(l.page(0).unwrap().start, 1206);
        assert_eq!(l.page(1).unwrap().start, 0);
        assert_eq!(l.page(2).unwrap().start, 0x100);
        assert_eq!(l.page(3).unwrap().start, 0x200);
        assert_eq!(l.page(4).unwrap().start, 0x29e);
        assert_eq!(l.page(5).unwrap().start, 0x39e);
        assert_eq!(l.page(6).unwrap().start, 0x49e);
        assert_eq!(l.size_without_page_zero(), 1206);
    }

    #[test]
    fn page_zero_read_plan() {
        let plan = bueib().read_plan(0).unwrap();
        let offs: Vec<_> = plan.iter().map(|c| (c.wire_offset, c.len, c.image_offset)).collect();
        assert_eq!(offs, vec![(0xFC, 1, 1206), (0xFD, 1, 1207), (0xFE, 1, 1208), (0xFF, 1, 1209)]);
    }

    #[test]
    fn page_read_plan_chunks_by_16() {
        let plan = bueib().read_plan(3).unwrap();
        assert_eq!(plan.len(), 10); // 158 = 9*16 + 14
        assert_eq!(plan[0], Chunk { page: 3, wire_offset: 0, len: 16, image_offset: 0x200 });
        assert_eq!(plan[9], Chunk { page: 3, wire_offset: 144, len: 14, image_offset: 0x200 + 144 });
        let total: usize = plan.iter().map(|c| c.len as usize).sum();
        assert_eq!(total, 158);
    }

    #[test]
    fn full_read_covers_image_once() {
        let l = bueib();
        let mut seen = vec![0u8; l.xsize];
        for p in &l.pages {
            for c in l.read_plan(p.nr).unwrap() {
                for i in c.image_range() {
                    seen[i] += 1;
                }
            }
        }
        assert!(seen.iter().all(|&n| n == 1));
    }

    #[test]
    fn layout_without_page_zero() {
        // BUECB (category 6, DDFI-2, 2003 XB9) has no page 0.
        let l = Layout::new(1138, &[(1, 256), (2, 256), (3, 90), (4, 256), (5, 256), (6, 24)]).unwrap();
        assert!(l.page(0).is_none());
        assert_eq!(l.size_without_page_zero(), 1138);
    }

    #[test]
    fn layout_errors() {
        assert!(matches!(Layout::new(10, &[(1, 4)]), Err(LayoutError::SizeMismatch { .. })));
        assert!(matches!(Layout::new(8, &[(1, 4), (1, 4)]), Err(LayoutError::DuplicatePage(1))));
        assert!(matches!(Layout::new(300, &[(1, 300)]), Err(LayoutError::BadPageLength { .. })));
    }

    #[test]
    fn write_plan() {
        let l = bueib();
        let plan = l.write_plan(2, 10, 20).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!((plan[0].wire_offset, plan[0].len, plan[0].image_offset), (10, 16, 0x100 + 10));
        assert_eq!((plan[1].wire_offset, plan[1].len), (26, 4));
        assert!(l.write_plan(0, 0, 1).is_err());
        assert!(l.write_plan(6, 20, 5).is_err());
        let image: Vec<u8> = (0..l.xsize).map(|i| i as u8).collect();
        let Request::Set { page, offset, data } = plan[1].set_request(&image) else { panic!() };
        assert_eq!((page, offset), (2, 26));
        assert_eq!(data, image[0x100 + 26..0x100 + 30].to_vec());
    }

    #[test]
    fn pages_touching_range() {
        let l = bueib();
        let nrs: Vec<_> = l.pages_touching(0xF0..0x110).map(|p| p.nr).collect();
        assert_eq!(nrs, vec![1, 2]);
    }
}
