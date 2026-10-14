// SPDX-License-Identifier: GPL-3.0-or-later
//! Decoding values from an EEPROM image or a runtime frame.
//!
//! Element decoding is ported from EcmDroid `Variable.refreshValue()` and
//! `Bit.refreshValue()` (`app/src/main/java/org/ecmdroid/`, commit
//! 2f17d29f5b08f67fb357ee8f2ec21e7ae930343e, Copyright (C) 2012 by Michel
//! Marti): elements are unsigned little-endian, `value = raw * scale +
//! translate`, negative offsets count from the end of the image. Row skip
//! bytes and (input, output) pairs are XBTune's reading of the data (see
//! `model.rs`).

use crate::model::{EeVar, RtKind, RtVar, Shape};

/// An EEPROM image as decoding sees it: the full `xsize` bytes, and whether
/// page 0 (stored at the end) holds real data.
#[derive(Debug, Clone, Copy)]
pub struct ImageView<'a> {
    pub bytes: &'a [u8],
    pub page_zero: bool,
}

/// A decoded EEPROM variable.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    List(Vec<f64>),
    /// `rows[r][c]`.
    Grid(Vec<Vec<f64>>),
    /// (input, output) pairs.
    Pairs(Vec<(f64, f64)>),
    /// One byte of flags.
    Bits(u8),
    Text(String),
}

/// Why a variable can't be decoded from an image.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("stored in page 0, which this image does not contain")]
    NoPageZero,
    #[error("outside the image")]
    OutOfBounds,
    #[error("layout not understood: {0}")]
    BadShape(String),
}

/// Read `width` bytes at `at` as an unsigned little-endian number.
pub fn le(bytes: &[u8], at: usize, width: usize) -> u32 {
    bytes[at..at + width].iter().rev().fold(0u32, |v, &b| (v << 8) | b as u32)
}

impl EeVar {
    /// Start of this variable in the flat image.
    pub fn start(&self, img: &ImageView) -> Result<usize, DecodeError> {
        let start = if self.offset < 0 {
            if !img.page_zero {
                return Err(DecodeError::NoPageZero);
            }
            img.bytes.len().checked_sub((-self.offset) as usize).ok_or(DecodeError::OutOfBounds)?
        } else {
            self.offset as usize
        };
        if start + self.size > img.bytes.len() {
            return Err(DecodeError::OutOfBounds);
        }
        Ok(start)
    }

    /// Image byte ranges of each element, in storage order (skip bytes left
    /// out). For a map that is row by row.
    pub fn element_offsets(&self, img: &ImageView) -> Result<Vec<usize>, DecodeError> {
        let start = self.start(img)?;
        let w = self.width;
        Ok(match &self.shape {
            Shape::Scalar | Shape::Bits => vec![start],
            Shape::List(n) => (0..*n).map(|i| start + i * w).collect(),
            Shape::Pairs { rows } => (0..rows * 2).map(|i| start + i * w).collect(),
            Shape::Grid { rows, cols, skip } => {
                let stride = cols * w + skip;
                (0..*rows).flat_map(|r| (0..*cols).map(move |c| start + r * stride + c * w)).collect()
            }
            Shape::Text => vec![start],
            Shape::Invalid(why) => return Err(DecodeError::BadShape(why.clone())),
        })
    }

    /// Raw (unscaled) element values in storage order.
    pub fn raw(&self, img: &ImageView) -> Result<Vec<u32>, DecodeError> {
        Ok(self.element_offsets(img)?.into_iter().map(|o| le(img.bytes, o, self.width)).collect())
    }

    /// Scale a raw output value.
    pub fn scaled(&self, raw: u32) -> f64 {
        raw as f64 * self.scale + self.translate
    }

    /// Scale a raw input value (first column of a pairs table).
    pub fn axis_scaled(&self, raw: u32) -> f64 {
        raw as f64 * self.axis_scale + self.axis_translate
    }

    /// Decode into engineering units.
    pub fn decode(&self, img: &ImageView) -> Result<Value, DecodeError> {
        if self.shape == Shape::Text {
            let start = self.start(img)?;
            let raw = &img.bytes[start..start + self.size];
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            return Ok(Value::Text(String::from_utf8_lossy(&raw[..end]).into_owned()));
        }
        let raw = self.raw(img)?;
        Ok(match &self.shape {
            Shape::Scalar => Value::Number(self.scaled(raw[0])),
            Shape::Bits => Value::Bits(raw[0] as u8),
            Shape::List(_) => Value::List(raw.iter().map(|&r| self.scaled(r)).collect()),
            Shape::Grid { cols, .. } => Value::Grid(
                raw.chunks(*cols).map(|row| row.iter().map(|&r| self.scaled(r)).collect()).collect(),
            ),
            Shape::Pairs { .. } => {
                Value::Pairs(raw.chunks(2).map(|p| (self.axis_scaled(p[0]), self.scaled(p[1]))).collect())
            }
            Shape::Text | Shape::Invalid(_) => unreachable!(),
        })
    }
}

impl RtVar {
    /// Raw value from a runtime frame (the whole reply frame).
    pub fn raw(&self, frame: &[u8]) -> Option<u32> {
        (self.offset + self.size <= frame.len() && self.size <= 4).then(|| le(frame, self.offset, self.size))
    }

    /// Value in engineering units (bitfields: the raw byte).
    pub fn value(&self, frame: &[u8]) -> Option<f64> {
        let raw = self.raw(frame)?;
        Some(match self.kind {
            RtKind::Scalar => raw as f64 * self.scale + self.translate,
            RtKind::Bitfield => raw as f64,
        })
    }
}

/// Short unit symbol, as EcmDroid's `Units.getSymbol()`.
pub fn unit_symbol(units: &str) -> Option<&'static str> {
    Some(match units {
        "Degree" | "Degree BTDC" | "Degree C" | "Degrees" | "Degrees BDD" | "Degrees C" | "TE degC" => "°",
        "Percent" => "%",
        "Volt" | "Volts" => "V",
        _ => return None,
    })
}

/// Format a number with `decimals` places (EcmDroid's DecimalFormat("0.00")
/// rounds half-even; this rounds half away from zero, which only differs on
/// exact ties).
pub fn format_number(v: f64, decimals: Option<u32>) -> String {
    match decimals {
        Some(d) => format!("{v:.*}", d as usize),
        None => {
            if v.fract() == 0.0 && v.abs() < 1e15 {
                format!("{v:.0}")
            } else {
                let s = format!("{v:.4}");
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            }
        }
    }
}
