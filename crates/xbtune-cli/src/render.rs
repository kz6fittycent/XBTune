// SPDX-License-Identifier: GPL-3.0-or-later
//! Text rendering of decoded EEPROM variables.

use xbtune_core::image::Image;
use xbtune_defs::{Definition, EeVar, ImageView, Shape, Value, format_number, unit_symbol};

fn unit(units: Option<&str>) -> String {
    match units {
        Some(u) => unit_symbol(u).map(str::to_string).unwrap_or_else(|| format!(" {u}")),
        None => String::new(),
    }
}

/// One-line value of a variable, or `None` for tables/maps.
pub fn inline_value(var: &EeVar, view: &ImageView) -> String {
    match var.decode(view) {
        Ok(Value::Number(x)) => {
            format!("{}{}", format_number(x, var.names.decimals), unit(var.names.units.as_deref()))
        }
        Ok(Value::Bits(b)) => {
            let set: Vec<&str> = var
                .bits
                .iter()
                .flat_map(|bs| bs.bits.iter())
                .filter(|bit| b & (1 << bit.nr) != 0)
                .map(|bit| bit.name.as_str())
                .collect();
            if set.is_empty() { format!("{b:08b}") } else { format!("{b:08b}  [{}]", set.join(", ")) }
        }
        Ok(Value::Text(t)) => format!("\"{t}\""),
        Ok(Value::List(v)) if v.len() <= 8 => {
            let u = unit(var.names.units.as_deref());
            v.iter()
                .map(|x| format!("{}{u}", format_number(*x, var.names.decimals)))
                .collect::<Vec<_>>()
                .join(", ")
        }
        Ok(Value::List(v)) => format!("[{} values]", v.len()),
        Ok(Value::Grid(r)) => format!("[{}x{} map]", r.len(), r.first().map_or(0, Vec::len)),
        Ok(Value::Pairs(p)) => format!("[{}-point table]", p.len()),
        Err(e) => format!("({e})"),
    }
}

/// Full rendering of a variable, multi-line for tables and maps.
pub fn render(var: &EeVar, def: &Definition, img: &Image) -> String {
    let view = img.view();
    let mut out = format!("{} - {}\n", var.name, var.label());
    if let Some(d) = var.names.description.as_ref().filter(|d| Some(*d) != var.names.label.as_ref()) {
        out.push_str(&format!("  {d}\n"));
    }
    let start = var.start(&view).map(|s| format!("{s:#06x}")).unwrap_or_else(|_| "page 0".into());
    out.push_str(&format!("  {:?} at {start}, {} bytes\n", var.kind, var.size));
    let d = var.names.decimals;
    let u = unit(var.names.units.as_deref());
    match var.decode(&view) {
        Ok(Value::Grid(rows)) => {
            let axis = |name: &Option<String>| -> Option<Vec<f64>> {
                let a = def.eeprom.get(name.as_ref()?)?;
                match a.decode(&view).ok()? {
                    Value::List(v) => Some(v),
                    _ => None,
                }
            };
            let xs = axis(&var.x_axis);
            let ys = axis(&var.y_axis);
            let xa = var.x_axis.as_ref().and_then(|n| def.eeprom.get(n));
            let ya = var.y_axis.as_ref().and_then(|n| def.eeprom.get(n));
            out.push_str(&format!(
                "  rows: {}   columns: {}   values in{}\n",
                ya.map_or("-", |a| a.label()),
                xa.map_or("-", |a| a.label()),
                if u.is_empty() { " raw units".into() } else { u.clone() }
            ));
            let w = 7;
            out.push_str(&format!("{:>w$} |", ""));
            if let Some(xs) = &xs {
                for x in xs {
                    out.push_str(&format!("{:>w$}", format_number(*x, xa.and_then(|a| a.names.decimals))));
                }
            }
            out.push('\n');
            for (r, row) in rows.iter().enumerate() {
                let label = ys
                    .as_ref()
                    .and_then(|y| y.get(r))
                    .map_or(String::new(), |y| format_number(*y, ya.and_then(|a| a.names.decimals)));
                out.push_str(&format!("{label:>w$} |"));
                for v in row {
                    out.push_str(&format!("{:>w$}", format_number(*v, d)));
                }
                out.push('\n');
            }
        }
        Ok(Value::Pairs(pairs)) => {
            let au = unit(var.names.axis_units.as_deref());
            out.push_str(
                "  input -> output   (input scaling is XBTune's reading of the data; unconfirmed)\n",
            );
            for (x, y) in pairs {
                out.push_str(&format!(
                    "  {:>10}{au} -> {}{u}\n",
                    format_number(x, var.names.axis_decimals),
                    format_number(y, d)
                ));
            }
        }
        Ok(Value::List(v)) => {
            for (i, x) in v.iter().enumerate() {
                out.push_str(&format!("  [{i:>2}] {}{u}\n", format_number(*x, d)));
            }
        }
        Ok(_) => out.push_str(&format!("  = {}\n", inline_value(var, &view))),
        Err(e) => out.push_str(&format!("  cannot decode: {e}\n")),
    }
    if let Shape::Invalid(why) = &var.shape {
        out.push_str(&format!("  note: {why}\n"));
    }
    out
}

/// The variable whose bytes include image offset `at`, if any.
pub fn var_at<'a>(def: &'a Definition, view: &ImageView, at: usize) -> Option<&'a EeVar> {
    def.eeprom.values().find(|v| v.start(view).is_ok_and(|s| at >= s && at < s + v.size))
}
