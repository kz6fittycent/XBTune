// SPDX-License-Identifier: GPL-3.0-or-later
//! Writing ("burning") EEPROM changes to the ECM, following KICKOFF §6.
//!
//! Wire protocol ported from EcmDroid (commit
//! 2f17d29f5b08f67fb357ee8f2ec21e7ae930343e, Copyright (C) 2012 by Michel
//! Marti): `ECM.writeEEPromPage()` sends SET requests of up to 16 bytes at
//! in-page offsets 0, 16, 32, ... (`PDU.setRequest()`), each needing an ACK
//! (`ECM.sendPDU()`); page 0 is never written whole, only the variables in
//! `PAGE_ZERO_VARS_TO_WRITE` (`LFuel1`, `KBaro`), each as one SET at wire
//! offset `0xFF + offset + 1`. `BurnTask` checks the connected ECM's version
//! starts with the image's ID before writing.
//!
//! XBTune additions (KICKOFF §6):
//! - a fresh verified full read, saved as a backup, before writing;
//! - only the 16-byte chunks that differ are sent (the same requests
//!   EcmDroid would send for that page, just fewer);
//! - by default only variables the user chose are written over the ECM's
//!   current contents, so bytes the ECM keeps updating itself (AFV, stored
//!   codes, counters) aren't put back to stale values from a file;
//! - every written page is read back and compared byte for byte;
//! - link health and a stopped engine are checked first;
//! - on failure the exact differing pages are reported.

use std::collections::BTreeSet;
use std::time::Duration;

use xbtune_defs::{Definition, EcmDef, EeVar, ImageView, Shape, Value, format_number, unit_symbol};
use xbtune_link::EcmClient;
use xbtune_proto::eeprom::{Chunk, Layout, PAGE_ZERO_END};
use xbtune_proto::{Request, TransactionError};

/// Page 0 variables EcmDroid writes (`ECM.PAGE_ZERO_VARS_TO_WRITE`).
pub const PAGE_ZERO_WRITABLE: &[&str] = &["LFuel1", "KBaro"];

/// AFV variables (`LFuel` on DDFI-2, also `LFuel1` on some DDFI-3) and the
/// neutral value an AFV reset stores.
pub const AFV_VARS: &[&str] = &["LFuel", "LFuel1"];
pub const AFV_NEUTRAL: f64 = 100.0;

/// One variable that differs between the ECM and the tune.
#[derive(Debug, Clone, PartialEq)]
pub struct VarChange {
    pub name: String,
    pub label: String,
    /// Human-readable values, e.g. "6800 RPM" or "[map: 12 cells differ]".
    pub ecm: String,
    pub tune: String,
    /// The user changed it while editing (pre-selected for writing).
    pub edited: bool,
    /// Why it can't be written, if so.
    pub blocked: Option<String>,
}

fn human(v: &EeVar, img: &ImageView) -> String {
    let u = v
        .names
        .units
        .as_deref()
        .map(|u| unit_symbol(u).map(str::to_string).unwrap_or_else(|| format!(" {u}")))
        .unwrap_or_default();
    match v.decode(img) {
        Ok(Value::Number(x)) => format!("{}{u}", format_number(x, v.names.decimals)),
        Ok(Value::Bits(b)) => format!("{b:08b}"),
        Ok(Value::Text(t)) => t,
        Ok(_) => String::new(),
        Err(e) => format!("({e})"),
    }
}

fn var_range(v: &EeVar, img: &ImageView) -> Option<std::ops::Range<usize>> {
    v.start(img).ok().map(|s| s..s + v.size)
}

/// Whether a variable may be written at all.
pub fn writable(v: &EeVar, img: &ImageView) -> Result<(), String> {
    if let Shape::Invalid(why) = &v.shape {
        return Err(format!("layout not understood ({why})"));
    }
    if v.offset < 0 && !PAGE_ZERO_WRITABLE.contains(&v.name.as_str()) {
        return Err("stored in page 0; only LFuel1 and KBaro can be written there".into());
    }
    if var_range(v, img).is_none() {
        return Err("not in this image".into());
    }
    Ok(())
}

/// Variables that differ between `ecm` (what the ECM holds now) and
/// `tune`. `base` is the tune as opened, to tell edits from bytes the ECM
/// changed by itself.
pub fn changes(
    def: &Definition,
    ecm: &[u8],
    tune: &[u8],
    base: Option<&[u8]>,
    page_zero: bool,
) -> Vec<VarChange> {
    let ev = ImageView { bytes: ecm, page_zero };
    let tv = ImageView { bytes: tune, page_zero };
    let mut out = Vec::new();
    let mut vars: Vec<&EeVar> = def.eeprom.values().collect();
    vars.sort_by_key(|v| v.start(&tv).unwrap_or(usize::MAX));
    for v in vars {
        let Some(r) = var_range(v, &tv) else { continue };
        if ecm[r.clone()] == tune[r.clone()] {
            continue;
        }
        let edited = base.is_none_or(|b| b[r.clone()] != tune[r.clone()]);
        let (e, t) = match &v.shape {
            Shape::Grid { .. } | Shape::List(_) | Shape::Pairs { .. } => {
                let n = v
                    .element_offsets(&tv)
                    .map(|o| o.iter().filter(|&&i| ecm[i..i + v.width] != tune[i..i + v.width]).count())
                    .unwrap_or(0);
                (String::new(), format!("{n} of {} elements differ", v.element_count()))
            }
            _ => (human(v, &ev), human(v, &tv)),
        };
        out.push(VarChange {
            name: v.name.clone(),
            label: v.label().to_string(),
            ecm: e,
            tune: t,
            edited,
            blocked: writable(v, &tv).err(),
        });
    }
    out
}

/// The ECM's bytes with the chosen variables taken from the tune.
pub fn build_target(
    def: &Definition,
    ecm: &[u8],
    tune: &[u8],
    page_zero: bool,
    vars: &[String],
) -> Result<Vec<u8>, String> {
    let tv = ImageView { bytes: tune, page_zero };
    let mut out = ecm.to_vec();
    for name in vars {
        let v = def.eeprom.get(name).ok_or_else(|| format!("no variable {name}"))?;
        writable(v, &tv).map_err(|e| format!("{name}: {e}"))?;
        let r = var_range(v, &tv).expect("checked");
        out[r.clone()].copy_from_slice(&tune[r]);
    }
    Ok(out)
}

/// One SET to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOp {
    pub page: u8,
    pub wire_offset: u8,
    pub image_offset: usize,
    pub data: Vec<u8>,
}

impl WriteOp {
    pub fn request(&self) -> Request {
        Request::Set { page: self.page, offset: self.wire_offset, data: self.data.clone() }
    }
}

/// What a burn will send.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub writes: Vec<WriteOp>,
    /// Pages touched (read back after writing).
    pub pages: BTreeSet<u8>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.writes.iter().map(|w| w.data.len()).sum()
    }
}

/// Plan the writes that turn `ecm` into `target`. Fails if a page 0 byte
/// outside the writable variables differs.
pub fn plan(layout: &Layout, def: &Definition, ecm: &[u8], target: &[u8]) -> Result<Plan, String> {
    if ecm.len() != target.len() || ecm.len() != layout.xsize {
        return Err(format!("image sizes differ ({} / {} / {})", ecm.len(), target.len(), layout.xsize));
    }
    let mut p = Plan::default();
    let mut nrs: Vec<u8> = layout.pages.iter().map(|pg| pg.nr).collect();
    nrs.sort_unstable();
    for nr in nrs {
        let page = *layout.page(nr).expect("listed");
        let range = page.start..page.start + page.len;
        if ecm[range.clone()] == target[range.clone()] {
            continue;
        }
        if nr == 0 {
            // Only LFuel1/KBaro, each as one SET.
            let view = ImageView { bytes: target, page_zero: true };
            let mut covered = vec![false; page.len];
            for name in PAGE_ZERO_WRITABLE {
                let Some(v) = def.eeprom.get(*name) else { continue };
                if v.offset >= 0 {
                    continue;
                }
                let Some(r) = var_range(v, &view) else { continue };
                for i in r.clone() {
                    covered[i - page.start] = true;
                }
                if ecm[r.clone()] != target[r.clone()] {
                    let wire = PAGE_ZERO_END as i64 + v.offset as i64 + 1;
                    p.writes.push(WriteOp {
                        page: 0,
                        wire_offset: wire as u8,
                        image_offset: r.start,
                        data: target[r].to_vec(),
                    });
                    p.pages.insert(0);
                }
            }
            let stray: Vec<usize> =
                range.clone().filter(|&i| ecm[i] != target[i] && !covered[i - page.start]).collect();
            if !stray.is_empty() {
                let names: BTreeSet<String> = stray
                    .iter()
                    .filter_map(|&i| {
                        def.eeprom
                            .values()
                            .find(|v| var_range(v, &view).is_some_and(|r| r.contains(&i)))
                            .map(|v| v.name.clone())
                    })
                    .collect();
                return Err(format!(
                    "page 0 bytes differ outside LFuel1/KBaro ({}); XBTune won't write them",
                    if names.is_empty() {
                        "unnamed bytes".into()
                    } else {
                        names.into_iter().collect::<Vec<_>>().join(", ")
                    }
                ));
            }
            continue;
        }
        let chunks: Vec<Chunk> = layout.write_plan(nr, 0, page.len).map_err(|e| e.to_string())?;
        for c in chunks {
            let r = c.image_range();
            if ecm[r.clone()] != target[r.clone()] {
                p.writes.push(WriteOp {
                    page: nr,
                    wire_offset: c.wire_offset,
                    image_offset: r.start,
                    data: target[r].to_vec(),
                });
                p.pages.insert(nr);
            }
        }
    }
    Ok(p)
}

/// Result of a link test before burning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkCheck {
    pub attempts: u32,
    pub failures: u32,
    pub rtt_avg: Option<Duration>,
}

/// Limits a link must meet before a burn.
#[derive(Debug, Clone, Copy)]
pub struct LinkLimits {
    pub pings: u32,
    pub max_failures: u32,
    pub max_rtt: Duration,
}

impl Default for LinkLimits {
    fn default() -> Self {
        Self { pings: 20, max_failures: 1, max_rtt: Duration::from_millis(300) }
    }
}

/// Send `limits.pings` version requests, one attempt each.
pub async fn check_link(client: &mut EcmClient, limits: &LinkLimits) -> LinkCheck {
    let policy = client.policy();
    client.set_policy(xbtune_proto::RetryPolicy { max_attempts: 1, ..policy });
    let mut failures = 0;
    let mut total = Duration::ZERO;
    let mut ok = 0u32;
    for _ in 0..limits.pings {
        let t = tokio::time::Instant::now();
        if client.version().await.is_ok() {
            ok += 1;
            total += t.elapsed();
        } else {
            failures += 1;
        }
    }
    client.set_policy(policy);
    LinkCheck { attempts: limits.pings, failures, rtt_avg: (ok > 0).then(|| total / ok) }
}

impl LinkCheck {
    pub fn verdict(&self, limits: &LinkLimits) -> Result<(), String> {
        let rtt = self.rtt_avg.map_or("-".into(), |d| format!("{} ms", d.as_millis()));
        if self.failures > limits.max_failures || self.rtt_avg.is_none_or(|r| r > limits.max_rtt) {
            return Err(format!(
                "the link is too weak to write safely: {} of {} test requests failed, average reply {rtt} \
                 (limits: at most {} failures, {} ms). Move the laptop closer to the bike, or use a USB cable.",
                self.failures,
                self.attempts,
                limits.max_failures,
                limits.max_rtt.as_millis()
            ));
        }
        Ok(())
    }
}

/// Progress through a burn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnStep {
    Writing { done: usize, total: usize, page: u8 },
    Verifying { page: u8 },
}

/// A page that doesn't hold what it should, with the differing image
/// offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMismatch {
    pub page: u8,
    pub ranges: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurnReport {
    pub writes: usize,
    pub bytes: usize,
    pub pages: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BurnError {
    #[error("writing page {page} at offset {offset:#04x} failed: {error}")]
    Write { page: u8, offset: u8, error: String, mismatched: Option<Vec<PageMismatch>> },
    #[error("read-back found {} page(s) that don't hold the new data", .0.len())]
    Verify(Vec<PageMismatch>),
    #[error("reading back page {page} failed: {error}")]
    ReadBack { page: u8, error: String },
}

impl BurnError {
    /// Pages known to differ from the target (when it could be checked).
    pub fn mismatched(&self) -> Option<&[PageMismatch]> {
        match self {
            Self::Write { mismatched, .. } => mismatched.as_deref(),
            Self::Verify(m) => Some(m),
            Self::ReadBack { .. } => None,
        }
    }
}

/// Read `page` and compare it with `target`. A chunk that differs is read
/// up to three times more, so a corrupted read isn't taken for a failed
/// write.
pub async fn verify_page(
    client: &mut EcmClient,
    layout: &Layout,
    page: u8,
    target: &[u8],
) -> Result<Option<PageMismatch>, TransactionError> {
    let mut bad = Vec::new();
    for c in layout.read_plan(page).map_err(|e| TransactionError::Io(e.to_string()))? {
        let r = c.image_range();
        let mut data = client.read_bytes(c.page, c.wire_offset, c.len).await?;
        let mut tries = 0;
        while data[..] != target[r.clone()] && tries < 3 {
            data = client.read_bytes(c.page, c.wire_offset, c.len).await?;
            tries += 1;
        }
        if data[..] != target[r.clone()] {
            for (i, (a, b)) in data.iter().zip(&target[r.clone()]).enumerate() {
                if a != b {
                    let at = r.start + i;
                    match bad.last_mut() {
                        Some((_, end)) if *end == at => *end = at + 1,
                        _ => bad.push((at, at + 1)),
                    }
                }
            }
        }
    }
    Ok((!bad.is_empty()).then_some(PageMismatch { page, ranges: bad }))
}

/// Check that the ECM still holds `expected` in the pages `plan` touches
/// (nothing changed since the backup read).
pub async fn still_matches(
    client: &mut EcmClient,
    layout: &Layout,
    plan: &Plan,
    expected: &[u8],
) -> Result<Vec<PageMismatch>, TransactionError> {
    let mut out = Vec::new();
    for &p in &plan.pages {
        if let Some(m) = verify_page(client, layout, p, expected).await? {
            out.push(m);
        }
    }
    Ok(out)
}

/// Send the plan's writes, then read back every touched page.
pub async fn execute(
    client: &mut EcmClient,
    layout: &Layout,
    plan: &Plan,
    target: &[u8],
    mut progress: impl FnMut(BurnStep),
) -> Result<BurnReport, BurnError> {
    let total = plan.writes.len();
    for (i, w) in plan.writes.iter().enumerate() {
        progress(BurnStep::Writing { done: i, total, page: w.page });
        if let Err(e) = client.transact(&w.request()).await {
            // Find out what the ECM holds now, if the link allows.
            let mut mismatched = Vec::new();
            let mut checked = true;
            for &p in &plan.pages {
                match verify_page(client, layout, p, target).await {
                    Ok(Some(m)) => mismatched.push(m),
                    Ok(None) => {}
                    Err(_) => {
                        checked = false;
                        break;
                    }
                }
            }
            return Err(BurnError::Write {
                page: w.page,
                offset: w.wire_offset,
                error: e.to_string(),
                mismatched: checked.then_some(mismatched),
            });
        }
    }
    progress(BurnStep::Writing { done: total, total, page: plan.writes.last().map_or(0, |w| w.page) });
    let mut bad = Vec::new();
    for &p in &plan.pages {
        progress(BurnStep::Verifying { page: p });
        match verify_page(client, layout, p, target).await {
            Ok(Some(m)) => bad.push(m),
            Ok(None) => {}
            Err(e) => return Err(BurnError::ReadBack { page: p, error: e.to_string() }),
        }
    }
    if !bad.is_empty() {
        return Err(BurnError::Verify(bad));
    }
    Ok(BurnReport { writes: total, bytes: plan.bytes(), pages: plan.pages.iter().copied().collect() })
}

/// The tune with AFV set to neutral (100%), for an AFV reset.
pub fn afv_reset(def: &Definition, ecm: &[u8], page_zero: bool) -> Result<(Vec<u8>, Vec<String>), String> {
    let view = ImageView { bytes: ecm, page_zero };
    let mut out = ecm.to_vec();
    let mut names = Vec::new();
    for name in AFV_VARS {
        let Some(v) = def.eeprom.get(*name) else { continue };
        if writable(v, &view).is_err() || v.shape != Shape::Scalar {
            continue;
        }
        let (raw, _) = crate::tune::encode(v, 0, AFV_NEUTRAL);
        let at = v.start(&view).map_err(|e| e.to_string())?;
        for i in 0..v.width {
            out[at + i] = (raw >> (8 * i)) as u8;
        }
        names.push(name.to_string());
    }
    if names.is_empty() {
        return Err("no AFV variable that can be written in this definition".into());
    }
    Ok((out, names))
}

/// Check that a connected ECM may receive `image_id`'s data
/// (`BurnTask`: the version must start with the image's ID).
pub fn check_identity(version: &str, image_id: &str, ecm: &EcmDef) -> Result<(), String> {
    if !version.starts_with(image_id) || ecm.id != image_id {
        return Err(format!("the ECM reports '{version}', but the tune is for {image_id}; writing refused"));
    }
    if !ecm.family.in_scope() {
        return Err(format!("{} ({}) is outside XBTune's scope; writing refused", ecm.id, ecm.family));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
