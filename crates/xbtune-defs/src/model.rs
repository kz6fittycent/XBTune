// SPDX-License-Identifier: GPL-3.0-or-later
//! Definition model: EEPROM variables, runtime variables and bit sets for one
//! ECM definition (one ecmdroid.db "category").
//!
//! Field meanings follow EcmDroid's `DatabaseVariableProvider.convert()` and
//! `DatabaseBitSetProvider.getBitSet()` (`app/src/main/java/org/ecmdroid/`,
//! commit 2f17d29f5b08f67fb357ee8f2ec21e7ae930343e, Copyright (C) 2012 by
//! Michel Marti). Shapes of tables and maps (row skip bytes, axis/value pairs)
//! are XBTune's reading of the database columns; EcmDroid shows these only as
//! raw bytes. See docs/PROVENANCE.md.

use std::collections::BTreeMap;

use serde::Deserialize;

/// EEPROM variable type (`eeoffsets.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EeKind {
    /// A single number (or a few bytes shown as numbers).
    Value,
    /// A list of numbers.
    Array,
    /// The breakpoints of a map axis.
    Axis,
    /// A small 2-column table of (input, output) pairs.
    Table,
    /// A 2D map with axes (fuel, spark, ...).
    Map,
    /// One byte of named flags.
    Bits,
    /// ASCII text.
    String,
}

impl EeKind {
    fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "value" => Self::Value,
            "array" => Self::Array,
            "axis" => Self::Axis,
            "table" => Self::Table,
            "map" => Self::Map,
            "bits" => Self::Bits,
            "string" => Self::String,
            _ => return None,
        })
    }
}

/// How an EEPROM variable's bytes are arranged. Computed and checked when the
/// definitions load; anything that doesn't add up becomes [`Shape::Invalid`]
/// and is shown as raw bytes only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// One element.
    Scalar,
    /// `n` elements back to back.
    List(usize),
    /// `rows` x `cols` elements; each row is followed by `skip` unused bytes.
    Grid { rows: usize, cols: usize, skip: usize },
    /// `rows` (input, output) pairs, stored interleaved.
    Pairs { rows: usize },
    /// One byte of flags.
    Bits,
    /// Text of `size` bytes, NUL-terminated.
    Text,
    /// The database fields don't describe a consistent layout.
    Invalid(String),
}

/// Display metadata from the `names` table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Names {
    pub label: Option<String>,
    pub description: Option<String>,
    pub remark: Option<String>,
    pub units: Option<String>,
    pub decimals: Option<u32>,
    pub axis_units: Option<String>,
    pub axis_decimals: Option<u32>,
}

/// One EEPROM variable.
#[derive(Debug, Clone, PartialEq)]
pub struct EeVar {
    /// Database variable name, e.g. `Tab_Fuel_Front`, `KTemp_Fan_On`.
    pub name: String,
    pub names: Names,
    pub kind: EeKind,
    /// Bytes covered, including row skip bytes.
    pub size: usize,
    /// Offset in the flat image. Negative offsets count from the end of the
    /// image (page 0), as in EcmDroid's `Variable.refreshValue()`.
    pub offset: i32,
    /// Bytes per element (little-endian).
    pub width: usize,
    pub scale: f64,
    pub translate: f64,
    /// Scale/translate of the input column of a [`Shape::Pairs`] table.
    pub axis_scale: f64,
    pub axis_translate: f64,
    /// Axis variables of a map: x runs along a row (columns), y down (rows).
    pub x_axis: Option<String>,
    pub y_axis: Option<String>,
    pub shape: Shape,
    /// Named flags, for [`EeKind::Bits`].
    pub bits: Option<BitSetDef>,
    /// Hidden by the original database ("secret").
    pub secret: bool,
}

impl EeVar {
    /// Human name: the database label, else the variable name.
    pub fn label(&self) -> &str {
        self.names.label.as_deref().unwrap_or(&self.name)
    }

    /// Number of elements (excluding skip bytes).
    pub fn element_count(&self) -> usize {
        match &self.shape {
            Shape::Scalar | Shape::Bits => 1,
            Shape::List(n) => *n,
            Shape::Grid { rows, cols, .. } => rows * cols,
            Shape::Pairs { rows } => rows * 2,
            Shape::Text | Shape::Invalid(_) => 0,
        }
    }
}

/// Runtime variable type (`rtoffsets.type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtKind {
    Scalar,
    Bitfield,
}

/// One runtime ("live") variable. Offsets are into the whole runtime-data
/// reply frame, header included, as in EcmDroid.
#[derive(Debug, Clone, PartialEq)]
pub struct RtVar {
    /// EcmDroid's channel name (`names.origname`), e.g. `RPM`, `pw1`, `AFV`.
    pub name: String,
    /// Database variable name.
    pub varname: String,
    pub names: Names,
    pub kind: RtKind,
    pub size: usize,
    pub offset: usize,
    pub scale: f64,
    pub translate: f64,
    /// Display range (already in engineering units).
    pub low: Option<f64>,
    pub high: Option<f64>,
    pub bits: Option<BitSetDef>,
}

impl RtVar {
    pub fn label(&self) -> &str {
        self.names.label.as_deref().unwrap_or(&self.name)
    }
}

/// One flag of a bit set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitDef {
    /// Bit number 0..=7 (bit 0 is the least significant).
    pub nr: u8,
    pub name: String,
    pub description: Option<String>,
    /// Trouble code this bit reports, if any.
    pub dtc: Option<String>,
}

/// A byte of named flags (`bits` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BitSetDef {
    pub label: Option<String>,
    pub bits: Vec<BitDef>,
}

/// Everything known about one ECM definition.
#[derive(Debug, Clone)]
pub struct Definition {
    pub category: u32,
    pub eeprom: BTreeMap<String, EeVar>,
    /// Keyed by EcmDroid channel name.
    pub runtime: BTreeMap<String, RtVar>,
    /// Variables left out because ecmdroid.db has conflicting rows for them.
    pub conflicts: Vec<String>,
    /// Problems found while checking the data (for `xbtune-cli defs`).
    pub issues: Vec<String>,
}

impl Definition {
    /// EEPROM variables in image order.
    pub fn eeprom_by_offset(&self, image_len: usize) -> Vec<&EeVar> {
        let mut v: Vec<_> = self.eeprom.values().collect();
        v.sort_by_key(|e| if e.offset < 0 { image_len as i64 + e.offset as i64 } else { e.offset as i64 });
        v
    }
}

// ---- Raw rows as stored in data/ecmdroid.toml ----

#[derive(Deserialize)]
pub(crate) struct EeRow {
    pub c: u32,
    pub var: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub size: usize,
    pub offset: i32,
    pub scale: Option<f64>,
    pub translate: Option<f64>,
    pub elemsize: Option<usize>,
    pub cols: Option<usize>,
    pub rows: Option<usize>,
    pub skip: Option<usize>,
    pub axis_scale: Option<f64>,
    pub axis_translate: Option<f64>,
    pub xaxis: Option<String>,
    pub yaxis: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct RtRow {
    pub c: u32,
    pub var: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub size: usize,
    pub offset: i32,
    pub scale: Option<f64>,
    pub translate: Option<f64>,
    pub low: Option<f64>,
    pub high: Option<f64>,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Deserialize)]
pub(crate) struct BitsRow {
    pub var: String,
    pub name: Option<String>,
    pub desc: Vec<String>,
    pub dtc: Vec<String>,
    pub bitname: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct NameRow {
    pub var: String,
    pub origname: Option<String>,
    pub label: Option<String>,
    pub remark: Option<String>,
    pub description: Option<String>,
    pub units: Option<String>,
    pub decimals: Option<u32>,
    pub axis_units: Option<String>,
    pub axis_decimals: Option<u32>,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Deserialize)]
pub(crate) struct Conflict {
    pub c: u32,
    pub var: String,
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_string)
}

impl NameRow {
    pub(crate) fn names(&self) -> Names {
        Names {
            label: non_empty(&self.label),
            description: non_empty(&self.description),
            remark: non_empty(&self.remark),
            units: non_empty(&self.units),
            decimals: self.decimals,
            axis_units: non_empty(&self.axis_units),
            axis_decimals: self.axis_decimals,
        }
    }
}

impl BitsRow {
    /// Mirrors `DatabaseBitSetProvider`: a bit exists if it has a name or a
    /// description; a missing name becomes `<set>.<1-based bit>`.
    pub(crate) fn to_def(&self) -> BitSetDef {
        let mut bits = Vec::new();
        for i in 0..8 {
            let name = self.bitname.get(i).map(|s| s.trim()).unwrap_or("");
            let desc = self.desc.get(i).map(|s| s.trim()).unwrap_or("");
            if name.is_empty() && desc.is_empty() {
                continue;
            }
            let dtc = self.dtc.get(i).map(|s| s.trim()).unwrap_or("");
            bits.push(BitDef {
                nr: i as u8,
                name: if name.is_empty() { format!("{}.{}", self.var, i + 1) } else { name.to_string() },
                description: (!desc.is_empty()).then(|| desc.to_string()),
                dtc: (!dtc.is_empty()).then(|| dtc.to_string()),
            });
        }
        BitSetDef { label: non_empty(&self.name), bits }
    }
}

/// `scale` of 0 or missing means "unscaled" (`Variable.refreshValue()` only
/// multiplies when scale != 0).
fn scale_or_one(v: Option<f64>) -> f64 {
    match v {
        Some(s) if s != 0.0 => s,
        _ => 1.0,
    }
}

impl EeRow {
    /// Build the variable and check its layout against the image.
    /// `size` is the image size without page 0, `zero` the page 0 length.
    pub(crate) fn to_var(
        &self,
        names: Names,
        bits: Option<BitSetDef>,
        size: usize,
        zero: usize,
    ) -> Option<EeVar> {
        let kind = EeKind::parse(&self.kind)?;
        // Variable.setWidth(): at least 1.
        let width = self.elemsize.unwrap_or(0).max(1);
        let shape = shape(kind, self, width, size, zero);
        Some(EeVar {
            name: self.var.clone(),
            names,
            kind,
            size: self.size,
            offset: self.offset,
            width,
            scale: scale_or_one(self.scale),
            translate: self.translate.unwrap_or(0.0),
            axis_scale: scale_or_one(self.axis_scale),
            axis_translate: self.axis_translate.unwrap_or(0.0),
            x_axis: non_empty(&self.xaxis),
            y_axis: non_empty(&self.yaxis),
            shape,
            bits,
            secret: false,
        })
    }
}

fn shape(kind: EeKind, r: &EeRow, width: usize, size: usize, zero: usize) -> Shape {
    // Bounds: positive offsets must lie in pages 1..N, negative ones in page 0.
    let in_bounds = if r.offset >= 0 {
        r.offset as usize + r.size <= size
    } else {
        let back = (-r.offset) as usize;
        back <= zero && r.size <= back
    };
    if !in_bounds {
        return Shape::Invalid(format!("offset {} + size {} is outside the EEPROM", r.offset, r.size));
    }
    if r.size == 0 {
        return Shape::Invalid("size 0".into());
    }
    if kind == EeKind::String {
        return Shape::Text;
    }
    if r.size % width != 0 {
        return Shape::Invalid(format!("size {} is not a multiple of element width {width}", r.size));
    }
    let n = r.size / width;
    let (rows, cols, skip) = (r.rows.unwrap_or(0), r.cols.unwrap_or(0), r.skip.unwrap_or(0));
    match kind {
        EeKind::Bits if r.size == 1 => Shape::Bits,
        EeKind::Bits => Shape::Invalid(format!("bit set of {} bytes", r.size)),
        EeKind::Value if n == 1 => Shape::Scalar,
        EeKind::Value | EeKind::Array | EeKind::Axis => Shape::List(n),
        EeKind::Map => {
            if rows > 0 && cols > 0 && rows * (cols * width + skip) == r.size {
                Shape::Grid { rows, cols, skip }
            } else {
                Shape::Invalid(format!("map {rows}x{cols} (+{skip} skip) doesn't fit {} bytes", r.size))
            }
        }
        EeKind::Table => {
            if cols == 2 && rows > 0 && rows * 2 * width == r.size && skip == 0 {
                Shape::Pairs { rows }
            } else {
                Shape::Invalid(format!("table {rows}x{cols} doesn't fit {} bytes", r.size))
            }
        }
        EeKind::String => unreachable!(),
    }
}

impl RtRow {
    pub(crate) fn to_var(&self, name: &NameRow, bits: Option<BitSetDef>) -> Option<RtVar> {
        let kind = match self.kind.to_ascii_lowercase().as_str() {
            "scalar" => RtKind::Scalar,
            "bitfield" => RtKind::Bitfield,
            _ => return None,
        };
        let channel = non_empty(&name.origname).unwrap_or_else(|| self.var.clone());
        let scale = scale_or_one(self.scale);
        let translate = self.translate.unwrap_or(0.0);
        // `high` is stored in engineering units in every row of the data
        // (checked against uhigh * scale + translate). EcmDroid's
        // Variable.getHigh() scales it again when uhigh is the full raw range,
        // which turns e.g. 87 ms into 0.116 ms; that is not ported.
        let high = self.high;
        Some(RtVar {
            name: channel,
            varname: self.var.clone(),
            names: name.names(),
            kind,
            size: self.size,
            offset: usize::try_from(self.offset).ok()?,
            scale,
            translate,
            low: self.low,
            high,
            bits,
        })
    }
}
