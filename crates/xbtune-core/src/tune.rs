// SPDX-License-Identifier: GPL-3.0-or-later
//! Offline tune editing: change an EEPROM image through its definition,
//! with exact undo/redo.
//!
//! Every edit is stored as byte patches (offset, old bytes, new bytes), so
//! undo restores the image bit for bit. Values are encoded back to raw with
//! the inverse of the decoding in `xbtune-defs`
//! (`raw = round((value - translate) / scale)`, unsigned little-endian;
//! EcmDroid `Variable.refreshValue()` read in reverse) and clamped to the
//! element's raw range. Grid operations (set, add, multiply, percent,
//! smooth, interpolate), the clipboard and linked front/rear editing are
//! XBTune's.
//!
//! Nothing here talks to the ECM.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use xbtune_defs::{Catalog, DecodeError, Definition, EcmDef, EeVar, Shape, Value};

use crate::image::{Image, ImageError, crc32};

/// One changed byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub offset: usize,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
}

/// One undoable step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub label: String,
    pub patches: Vec<Patch>,
}

/// What an edit did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    /// Elements whose bytes changed.
    pub changed: usize,
    /// Values outside the element's range, stored as the nearest limit.
    pub clamped: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum TuneError {
    #[error("no variable '{0}' in this definition")]
    NoVar(String),
    #[error("{0} can't be edited: {1}")]
    NotEditable(String, String),
    #[error("{0}: element {1} doesn't exist")]
    BadIndex(String, usize),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error("{0}")]
    Other(String),
}

/// An image open for editing.
#[derive(Debug, Clone)]
pub struct Tune {
    pub image: Image,
    pub ecm: EcmDef,
    pub def: Arc<Definition>,
    /// Where it was opened from or last saved to.
    pub path: Option<PathBuf>,
    /// Bytes as opened or last saved, for "changed" markers.
    saved: Vec<u8>,
    /// Bytes as first opened (for `parent_crc32`, and to tell the user's
    /// edits from bytes the ECM changed by itself when burning).
    opened: Vec<u8>,
    opened_crc: u32,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

impl Tune {
    pub fn new(image: Image, catalog: &Catalog) -> Result<Self, TuneError> {
        let ecm = catalog
            .lookup(&image.ecm_id)
            .cloned()
            .ok_or_else(|| ImageError::UnknownEcm(image.ecm_id.clone()))?;
        let def = catalog
            .definition(&ecm)
            .cloned()
            .ok_or_else(|| TuneError::Other(format!("{}: no definition", ecm.id)))?;
        let saved = image.bytes.clone();
        let opened_crc = crc32(&saved);
        let opened = saved.clone();
        Ok(Self {
            image,
            ecm,
            def,
            path: None,
            saved,
            opened,
            opened_crc,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    pub fn open(path: &Path, catalog: &Catalog) -> Result<Self, TuneError> {
        let image = Image::load(path, None, catalog)?;
        let mut t = Self::new(image, catalog)?;
        t.path = Some(path.to_path_buf());
        Ok(t)
    }

    pub fn var(&self, name: &str) -> Result<&EeVar, TuneError> {
        self.def.eeprom.get(name).ok_or_else(|| TuneError::NoVar(name.into()))
    }

    pub fn value(&self, name: &str) -> Result<Value, TuneError> {
        Ok(self.var(name)?.decode(&self.image.view())?)
    }

    /// Element values in storage order (pairs: input, output, input, ...).
    pub fn elements(&self, name: &str) -> Result<Vec<f64>, TuneError> {
        let v = self.var(name)?;
        let raw = v.raw(&self.image.view())?;
        Ok(raw.iter().enumerate().map(|(i, r)| element_value(v, i, *r)).collect())
    }

    /// The same, as they were when opened or last saved.
    pub fn saved_elements(&self, name: &str) -> Result<Vec<f64>, TuneError> {
        let v = self.var(name)?;
        let view = xbtune_defs::ImageView { bytes: &self.saved, page_zero: self.image.page_zero };
        let raw = v.raw(&view)?;
        Ok(raw.iter().enumerate().map(|(i, r)| element_value(v, i, *r)).collect())
    }

    /// Whether `name` can be edited, and why not.
    pub fn editable(&self, name: &str) -> Result<(), TuneError> {
        let v = self.var(name)?;
        let why = match &v.shape {
            Shape::Invalid(why) => Some(format!("layout not understood ({why})")),
            Shape::Text => Some("text".into()),
            _ if v.start(&self.image.view()).is_err() => {
                Some("stored in page 0, which this image lacks".into())
            }
            _ => None,
        };
        match why {
            Some(w) => Err(TuneError::NotEditable(name.into(), w)),
            None => Ok(()),
        }
    }

    /// Set elements (`index` in storage order) to values in engineering
    /// units, as one undoable step.
    pub fn set(
        &mut self,
        label: impl Into<String>,
        changes: &[(String, usize, f64)],
    ) -> Result<Applied, TuneError> {
        let mut applied = Applied::default();
        let mut patches = Vec::new();
        for (name, index, value) in changes {
            self.editable(name)?;
            let v = self.var(name)?.clone();
            let offsets = v.element_offsets(&self.image.view())?;
            let at = *offsets.get(*index).ok_or_else(|| TuneError::BadIndex(name.clone(), *index))?;
            let (raw, clamped) = encode(&v, *index, *value);
            if clamped {
                applied.clamped += 1;
            }
            let new: Vec<u8> = (0..v.width).map(|i| (raw >> (8 * i)) as u8).collect();
            let old = self.image.bytes[at..at + v.width].to_vec();
            if old != new {
                self.image.bytes[at..at + v.width].copy_from_slice(&new);
                patches.push(Patch { offset: at, old, new });
                applied.changed += 1;
            }
        }
        if !patches.is_empty() {
            self.undo.push(Edit { label: label.into(), patches });
            self.redo.clear();
        }
        Ok(applied)
    }

    /// Set a bits variable's byte.
    pub fn set_bits(&mut self, name: &str, byte: u8) -> Result<Applied, TuneError> {
        let v = self.var(name)?;
        if v.shape != Shape::Bits {
            return Err(TuneError::NotEditable(name.into(), "not a flags byte".into()));
        }
        self.set(format!("{name} flags"), &[(name.to_string(), 0, f64::from(byte))])
    }

    /// Copy a variable's bytes from another image of the same ECM.
    pub fn copy_from(&mut self, other: &Image, name: &str) -> Result<Applied, TuneError> {
        if other.ecm_id != self.image.ecm_id {
            return Err(TuneError::Other(format!(
                "the other image is a {}, this one a {}",
                other.ecm_id, self.image.ecm_id
            )));
        }
        self.editable(name)?;
        let v = self.var(name)?.clone();
        let theirs = v.raw(&other.view())?;
        let changes: Vec<_> =
            theirs.iter().enumerate().map(|(i, r)| (name.to_string(), i, element_value(&v, i, *r))).collect();
        self.set(format!("copy {name} from other image"), &changes)
    }

    /// Write raw byte patches `(offset, new bytes)` as one undoable step
    /// (remote tuning: an edit from the other side). Out-of-range patches
    /// are refused before anything changes. Returns the bytes changed.
    pub fn apply_patches(
        &mut self,
        label: impl Into<String>,
        patches: &[(usize, Vec<u8>)],
    ) -> Result<usize, TuneError> {
        let len = self.image.bytes.len();
        if let Some((o, b)) = patches.iter().find(|(o, b)| o.checked_add(b.len()).is_none_or(|end| end > len))
        {
            return Err(TuneError::Other(format!(
                "edit outside the image ({} bytes at {o}, image {len})",
                b.len()
            )));
        }
        let mut done = Vec::new();
        let mut changed = 0;
        for (offset, new) in patches {
            let old = self.image.bytes[*offset..offset + new.len()].to_vec();
            if old != *new {
                changed += old.iter().zip(new).filter(|(a, b)| a != b).count();
                self.image.bytes[*offset..offset + new.len()].copy_from_slice(new);
                done.push(Patch { offset: *offset, old, new: new.clone() });
            }
        }
        if !done.is_empty() {
            self.undo.push(Edit { label: label.into(), patches: done });
            self.redo.clear();
        }
        Ok(changed)
    }

    /// Replace the working bytes from outside (remote tuning: follow the
    /// other side's copy). Undo history no longer fits, so it's cleared.
    pub fn replace_bytes(&mut self, bytes: Vec<u8>) {
        if bytes.len() == self.image.bytes.len() && bytes != self.image.bytes {
            self.image.bytes = bytes;
            self.undo.clear();
            self.redo.clear();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Label of the step `undo` would revert.
    /// Labels of the undoable steps, oldest first.
    pub fn undo_labels(&self) -> impl Iterator<Item = &str> {
        self.undo.iter().map(|e| e.label.as_str())
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    pub fn undo(&mut self) -> Option<String> {
        let e = self.undo.pop()?;
        for p in e.patches.iter().rev() {
            self.image.bytes[p.offset..p.offset + p.old.len()].copy_from_slice(&p.old);
        }
        let label = e.label.clone();
        self.redo.push(e);
        Some(label)
    }

    pub fn redo(&mut self) -> Option<String> {
        let e = self.redo.pop()?;
        for p in &e.patches {
            self.image.bytes[p.offset..p.offset + p.new.len()].copy_from_slice(&p.new);
        }
        let label = e.label.clone();
        self.undo.push(e);
        Some(label)
    }

    /// The bytes as first opened.
    pub fn opened_bytes(&self) -> &[u8] {
        &self.opened
    }

    /// True if the bytes differ from the last save.
    pub fn is_modified(&self) -> bool {
        self.image.bytes != self.saved
    }

    /// Variables whose bytes differ from the last save.
    pub fn modified_vars(&self) -> Vec<String> {
        let view = self.image.view();
        let ranges = crate::image::diff_ranges(&self.saved, &self.image.bytes);
        self.def
            .eeprom
            .values()
            .filter(|v| v.start(&view).is_ok_and(|s| ranges.iter().any(|(a, b)| *a < s + v.size && *b > s)))
            .map(|v| v.name.clone())
            .collect()
    }

    /// Save to `path` (`.xbt` or `.xpr`), stamping the edit metadata.
    pub fn save(&mut self, path: &Path, catalog: &Catalog) -> Result<(), TuneError> {
        if self.is_modified()
            || self.image.meta.parent_crc32.is_empty() && crc32(&self.image.bytes) != self.opened_crc
        {
            self.image.meta.modified = crate::time::rfc3339(std::time::SystemTime::now());
            self.image.meta.parent_crc32 = format!("{:08x}", self.opened_crc);
        }
        self.image.save(path, catalog)?;
        self.saved = self.image.bytes.clone();
        self.path = Some(path.to_path_buf());
        Ok(())
    }
}

/// Value of element `index` from its raw number (pairs: even = input).
pub fn element_value(v: &EeVar, index: usize, raw: u32) -> f64 {
    if matches!(v.shape, Shape::Pairs { .. }) && index.is_multiple_of(2) {
        v.axis_scaled(raw)
    } else {
        v.scaled(raw)
    }
}

/// Raw number for `value` at element `index`, and whether it was clamped.
pub fn encode(v: &EeVar, index: usize, value: f64) -> (u32, bool) {
    let (scale, translate) = if matches!(v.shape, Shape::Pairs { .. }) && index.is_multiple_of(2) {
        (v.axis_scale, v.axis_translate)
    } else {
        (v.scale, v.translate)
    };
    let max = if v.width >= 4 { u32::MAX as f64 } else { ((1u64 << (8 * v.width)) - 1) as f64 };
    let raw = ((value - translate) / scale).round();
    if !raw.is_finite() {
        return (0, true);
    }
    let clamped = raw < 0.0 || raw > max;
    (raw.clamp(0.0, max) as u32, clamped)
}

/// A grid operation on selected cells.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Set(f64),
    Add(f64),
    Multiply(f64),
    /// Change by this many percent (+5 = x1.05).
    Percent(f64),
    /// Average each cell with its four neighbours.
    Smooth,
    /// Straight lines between the leftmost and rightmost selected cell of
    /// each row.
    InterpolateRows,
    /// The same down each column.
    InterpolateCols,
}

/// New values for the selected `cells` of `grid` (`(row, col)`).
pub fn apply_op(grid: &[Vec<f64>], cells: &[(usize, usize)], op: Op) -> Vec<((usize, usize), f64)> {
    let get = |r: usize, c: usize| grid.get(r).and_then(|row| row.get(c)).copied();
    let mut out = Vec::new();
    match op {
        Op::Set(x) => out.extend(cells.iter().map(|&rc| (rc, x))),
        Op::Add(x) => out.extend(cells.iter().filter_map(|&(r, c)| get(r, c).map(|v| ((r, c), v + x)))),
        Op::Multiply(x) => out.extend(cells.iter().filter_map(|&(r, c)| get(r, c).map(|v| ((r, c), v * x)))),
        Op::Percent(p) => {
            out.extend(cells.iter().filter_map(|&(r, c)| get(r, c).map(|v| ((r, c), v * (1.0 + p / 100.0)))))
        }
        Op::Smooth => {
            for &(r, c) in cells {
                let mut sum = 0.0;
                let mut n = 0.0;
                for (dr, dc) in [(0i64, 0i64), (-1, 0), (1, 0), (0, -1), (0, 1)] {
                    let (rr, cc) = (r as i64 + dr, c as i64 + dc);
                    if rr >= 0
                        && cc >= 0
                        && let Some(v) = get(rr as usize, cc as usize)
                    {
                        sum += v;
                        n += 1.0;
                    }
                }
                if n > 0.0 {
                    out.push(((r, c), sum / n));
                }
            }
        }
        Op::InterpolateRows | Op::InterpolateCols => {
            let rows = op == Op::InterpolateRows;
            let mut lines = std::collections::BTreeMap::<usize, Vec<usize>>::new();
            for &(r, c) in cells {
                let (line, pos) = if rows { (r, c) } else { (c, r) };
                lines.entry(line).or_default().push(pos);
            }
            for (line, mut pos) in lines {
                pos.sort_unstable();
                let (a, b) = (pos[0], *pos.last().expect("non-empty"));
                if b <= a {
                    continue;
                }
                let at = |p: usize| if rows { get(line, p) } else { get(p, line) };
                let (Some(va), Some(vb)) = (at(a), at(b)) else { continue };
                for &p in &pos {
                    let v = va + (vb - va) * (p - a) as f64 / (b - a) as f64;
                    out.push((if rows { (line, p) } else { (p, line) }, v));
                }
            }
        }
    }
    out
}

/// Cells copied from a grid, as rows of values.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub rows: Vec<Vec<f64>>,
}

impl Clip {
    /// The bounding box of `cells` (missing cells are skipped on paste).
    pub fn copy(grid: &[Vec<f64>], cells: &[(usize, usize)]) -> Option<Self> {
        let r0 = cells.iter().map(|c| c.0).min()?;
        let r1 = cells.iter().map(|c| c.0).max()?;
        let c0 = cells.iter().map(|c| c.1).min()?;
        let c1 = cells.iter().map(|c| c.1).max()?;
        let rows = (r0..=r1)
            .map(|r| {
                (c0..=c1)
                    .map(|c| grid.get(r).and_then(|row| row.get(c)).copied().unwrap_or(f64::NAN))
                    .collect()
            })
            .collect();
        Some(Self { rows })
    }

    /// Cells to set when pasting with the top-left at `at`, inside a
    /// `rows` x `cols` grid.
    pub fn paste_at(&self, at: (usize, usize), rows: usize, cols: usize) -> Vec<((usize, usize), f64)> {
        let mut out = Vec::new();
        for (i, row) in self.rows.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let (r, c) = (at.0 + i, at.1 + j);
                if r < rows && c < cols && v.is_finite() {
                    out.push(((r, c), *v));
                }
            }
        }
        out
    }

    /// Tab-separated text, one line per row (for the system clipboard).
    pub fn to_tsv(&self, decimals: Option<u32>) -> String {
        self.rows
            .iter()
            .map(|r| {
                r.iter().map(|v| xbtune_defs::format_number(*v, decimals)).collect::<Vec<_>>().join("\t")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Parse tab-, comma- or space-separated numbers.
    pub fn from_text(text: &str) -> Option<Self> {
        let rows: Vec<Vec<f64>> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                l.split(['\t', ',', ' ', ';'])
                    .filter(|t| !t.is_empty())
                    .map(|t| t.trim().parse().unwrap_or(f64::NAN))
                    .collect()
            })
            .collect();
        (!rows.is_empty() && rows.iter().all(|r| !r.is_empty())).then_some(Self { rows })
    }
}

/// The front/rear twin of a variable with the same shape (`Tab_Fuel_Front`
/// <-> `Tab_Fuel_Rear`), for linked editing.
pub fn partner(def: &Definition, name: &str) -> Option<String> {
    let swapped = if name.contains("Front") {
        name.replace("Front", "Rear")
    } else if name.contains("Rear") {
        name.replace("Rear", "Front")
    } else {
        return None;
    };
    let (a, b) = (def.eeprom.get(name)?, def.eeprom.get(&swapped)?);
    (a.shape == b.shape && a.width == b.width).then_some(swapped)
}

#[cfg(test)]
mod tests {

    use super::*;

    fn owner_tune() -> Tune {
        let c = Catalog::builtin().unwrap();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/owner/BUEIB_xbtune.xpr");
        let img = if p.exists() {
            Image::load(&p, Some("BUEIB"), &c).unwrap()
        } else {
            Image::from_raw(c.lookup("BUEIB").unwrap(), xbtune_sim::BUEIB_EEPROM).unwrap()
        };
        Tune::new(img, &c).unwrap()
    }

    #[test]
    fn scalar_edit_round_trips_and_undoes_exactly() {
        let mut t = owner_tune();
        let before = t.image.bytes.clone();
        let old = t.elements("KRPM_Hard_Hi").unwrap()[0];
        let a = t.set("rev limit", &[("KRPM_Hard_Hi".into(), 0, old + 100.0)]).unwrap();
        assert_eq!(a, Applied { changed: 1, clamped: 0 });
        assert_eq!(t.elements("KRPM_Hard_Hi").unwrap()[0], old + 100.0);
        assert!(t.is_modified());
        assert_eq!(t.modified_vars(), vec!["KRPM_Hard_Hi".to_string()]);
        assert_eq!(t.undo_label(), Some("rev limit"));
        assert_eq!(t.undo().as_deref(), Some("rev limit"));
        assert_eq!(t.image.bytes, before);
        assert!(!t.is_modified());
        t.redo();
        assert_eq!(t.elements("KRPM_Hard_Hi").unwrap()[0], old + 100.0);
        // A new edit clears redo.
        t.undo();
        t.set("other", &[("KRPM_Soft_Hi".into(), 0, 7000.0)]).unwrap();
        assert!(!t.can_redo());
    }

    #[test]
    fn values_are_quantised_and_clamped() {
        let mut t = owner_tune();
        let v = t.var("KRPM_Hard_Hi").unwrap().clone();
        // RPM limits are stored in steps of `scale` (50 RPM on BUEIB).
        let (raw, cl) = encode(&v, 0, 7523.0);
        assert!(!cl);
        assert_eq!(v.scaled(raw), (7523.0 / v.scale).round() * v.scale + v.translate);
        let a = t.set("too high", &[("KRPM_Hard_Hi".into(), 0, 1e9)]).unwrap();
        assert_eq!(a.clamped, 1);
        let max = ((1u64 << (8 * v.width)) - 1) as f64 * v.scale + v.translate;
        assert_eq!(t.elements("KRPM_Hard_Hi").unwrap()[0], max);
        let a = t.set("negative", &[("KRPM_Hard_Hi".into(), 0, -500.0)]).unwrap();
        assert_eq!(a.clamped, 1);
    }

    #[test]
    fn map_ops_on_the_owners_fuel_map() {
        let mut t = owner_tune();
        let Value::Grid(g) = t.value("Tab_Fuel_Front").unwrap() else { panic!() };
        let cols = g[0].len();
        let cells = [(0, 0), (0, 1), (1, 0)];
        let changes = apply_op(&g, &cells, Op::Add(5.0));
        assert_eq!(changes, vec![((0, 0), g[0][0] + 5.0), ((0, 1), g[0][1] + 5.0), ((1, 0), g[1][0] + 5.0)]);
        let list: Vec<_> =
            changes.iter().map(|((r, c), v)| ("Tab_Fuel_Front".to_string(), r * cols + c, *v)).collect();
        assert_eq!(t.set("+5", &list).unwrap().changed, 3);
        let Value::Grid(g2) = t.value("Tab_Fuel_Front").unwrap() else { panic!() };
        assert_eq!(g2[0][0], g[0][0] + 5.0);
        assert_eq!(g2[2][2], g[2][2]);
        // Only the map changed, and undo restores it.
        assert_eq!(t.modified_vars(), vec!["Tab_Fuel_Front".to_string()]);
        t.undo();
        assert!(!t.is_modified());
    }

    #[test]
    fn grid_operations() {
        let g = vec![vec![10.0, 20.0, 30.0, 40.0], vec![0.0, 0.0, 0.0, 0.0], vec![50.0, 50.0, 50.0, 50.0]];
        assert_eq!(apply_op(&g, &[(0, 0)], Op::Set(7.0)), vec![((0, 0), 7.0)]);
        assert_eq!(apply_op(&g, &[(0, 1)], Op::Multiply(1.5)), vec![((0, 1), 30.0)]);
        assert_eq!(apply_op(&g, &[(0, 3)], Op::Percent(-25.0)), vec![((0, 3), 30.0)]);
        // Smooth (1,1): itself 0, up 20, down 50, left 0, right 0 -> 14.
        assert_eq!(apply_op(&g, &[(1, 1)], Op::Smooth), vec![((1, 1), 14.0)]);
        // Corner has 3 cells: (10 + 0 + 20) / 3.
        assert_eq!(apply_op(&g, &[(0, 0)], Op::Smooth), vec![((0, 0), 10.0)]);
        // Row interpolation between first and last selected cell.
        let row: Vec<_> = (0..4).map(|c| (1, c)).collect();
        let mut g2 = g.clone();
        g2[1] = vec![100.0, 0.0, 0.0, 400.0];
        assert_eq!(
            apply_op(&g2, &row, Op::InterpolateRows),
            vec![((1, 0), 100.0), ((1, 1), 200.0), ((1, 2), 300.0), ((1, 3), 400.0)]
        );
        // Column interpolation.
        let col: Vec<_> = (0..3).map(|r| (r, 0)).collect();
        assert_eq!(
            apply_op(&g, &col, Op::InterpolateCols),
            vec![((0, 0), 10.0), ((1, 0), 30.0), ((2, 0), 50.0)]
        );
        // A single cell can't be interpolated.
        assert!(apply_op(&g, &[(1, 1)], Op::InterpolateRows).is_empty());
    }

    #[test]
    fn clipboard() {
        let g = vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]];
        let clip = Clip::copy(&g, &[(0, 1), (1, 2)]).unwrap();
        assert_eq!(clip.rows, vec![vec![2.0, 3.0], vec![5.0, 6.0]]);
        assert_eq!(clip.to_tsv(Some(0)), "2\t3\n5\t6");
        assert_eq!(Clip::from_text("2\t3\n5,6\n").unwrap(), clip);
        // Pasting near the edge drops what doesn't fit.
        assert_eq!(clip.paste_at((1, 2), 2, 3), vec![((1, 2), 2.0)]);
        assert!(Clip::from_text("").is_none());
    }

    #[test]
    fn bits_pairs_partner_and_copy() {
        let mut t = owner_tune();
        let c = Catalog::builtin().unwrap();
        // Flags byte.
        let name = t
            .def
            .eeprom
            .values()
            .find(|v| v.shape == Shape::Bits && t.editable(&v.name).is_ok())
            .unwrap()
            .name
            .clone();
        t.set_bits(&name, 0b1010_0101).unwrap();
        assert_eq!(t.value(&name).unwrap(), Value::Bits(0b1010_0101));
        // Pairs: input uses axis scaling.
        let pairs = t
            .def
            .eeprom
            .values()
            .find(|v| matches!(v.shape, Shape::Pairs { .. }) && t.editable(&v.name).is_ok())
            .unwrap()
            .name
            .clone();
        let el = t.elements(&pairs).unwrap();
        let Value::Pairs(p) = t.value(&pairs).unwrap() else { panic!() };
        assert_eq!((el[0], el[1]), p[0]);
        // Linked front/rear.
        assert_eq!(partner(&t.def, "Tab_Fuel_Front").as_deref(), Some("Tab_Fuel_Rear"));
        assert_eq!(partner(&t.def, "Tab_Spark_Rear").as_deref(), Some("Tab_Spark_Front"));
        assert_eq!(partner(&t.def, "KRPM_Hard_Hi"), None);
        // Copy a table from another image, undoably.
        let other = Image::from_raw(c.lookup("BUEIB").unwrap(), xbtune_sim::BUEIB_EEPROM).unwrap();
        let before = t.elements("Tab_Fuel_Front").unwrap();
        t.copy_from(&other, "Tab_Fuel_Front").unwrap();
        let other_t = Tune::new(other, &c).unwrap();
        assert_eq!(t.elements("Tab_Fuel_Front").unwrap(), other_t.elements("Tab_Fuel_Front").unwrap());
        t.undo();
        assert_eq!(t.elements("Tab_Fuel_Front").unwrap(), before);
        // A different ECM is refused.
        let bue2d = Image::from_raw(
            c.lookup("BUE2D").unwrap(),
            include_bytes!("../../../tests/fixtures/BUE2D.eeprom"),
        )
        .unwrap();
        assert!(t.copy_from(&bue2d, "Tab_Fuel_Front").is_err());
    }

    #[test]
    fn save_stamps_metadata_and_reloads() {
        let c = Catalog::builtin().unwrap();
        let mut t = owner_tune();
        let parent = crc32(&t.image.bytes);
        t.set("rev", &[("KRPM_Hard_Hi".into(), 0, 7000.0)]).unwrap();
        let dir = std::env::temp_dir().join(format!("xbtune-tune-{}", std::process::id()));
        let p = dir.join("edited.xbt");
        t.save(&p, &c).unwrap();
        assert!(!t.is_modified());
        let back = Tune::open(&p, &c).unwrap();
        assert_eq!(back.elements("KRPM_Hard_Hi").unwrap()[0], 7000.0);
        assert_eq!(back.image.meta.parent_crc32, format!("{parent:08x}"));
        assert!(!back.image.meta.modified.is_empty());
        // .xpr too (EcmDroid can open it).
        let x = dir.join("BUEIB_edited.xpr");
        t.save(&x, &c).unwrap();
        assert_eq!(Tune::open(&x, &c).unwrap().image.bytes, t.image.bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_and_page0_vars_are_not_editable() {
        let c = Catalog::builtin().unwrap();
        let img = Image::from_raw(c.lookup("BUEIB").unwrap(), xbtune_sim::BUEIB_EEPROM).unwrap();
        let t = Tune::new(img, &c).unwrap();
        assert!(!t.image.page_zero);
        let p0 = t.def.eeprom.values().find(|v| v.offset < 0).unwrap().name.clone();
        assert!(matches!(t.editable(&p0), Err(TuneError::NotEditable(..))));
        assert!(t.editable("Tab_Fuel_Front").is_ok());
        assert!(matches!(t.editable("Nope"), Err(TuneError::NoVar(_))));
    }

    #[test]
    fn remote_patches_are_one_undo_step_and_bounded() {
        let mut t = owner_tune();
        let len = t.image.bytes.len();
        assert!(t.apply_patches("bad", &[(len - 1, vec![1, 2])]).is_err());
        assert!(t.apply_patches("bad", &[(usize::MAX, vec![1])]).is_err());
        assert!(!t.can_undo());
        let before = t.image.bytes.clone();
        assert_eq!(t.apply_patches("Tuner: two cells", &[(10, vec![5, 6]), (20, vec![7])]).unwrap(), 3);
        assert_eq!(t.undo_label(), Some("Tuner: two cells"));
        assert_eq!(&t.image.bytes[10..12], &[5, 6]);
        t.undo();
        assert_eq!(t.image.bytes, before);
        let mut other = before.clone();
        other[3] = 9;
        t.replace_bytes(other.clone());
        assert_eq!(t.image.bytes, other);
        assert!(!t.can_undo() && !t.can_redo());
    }
}
