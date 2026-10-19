// SPDX-License-Identifier: GPL-3.0-or-later
//! Air at the bike's altitude (owner's request, 2026-10-19: "a big deal
//! for many who live in mountains").
//!
//! Offline: standard-atmosphere pressure and air density from altitude
//! (ISA troposphere, ICAO: P = 101.325 kPa x (1 - 2.25577e-5 h)^5.25588,
//! T = 15 °C - 6.5 °C/km). Online, only on the user's click: Open-Meteo
//! (open-meteo.com, free, no key) turns a town name into its elevation and
//! gives the current surface pressure and temperature there. What goes to
//! Open-Meteo: the town name typed, then its coordinates rounded to 0.01°
//! (about 1 km). The advisor only ever gets the numbers (elevation,
//! pressure, temperature, density), never the place.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::AiError;

/// Sea-level standard pressure, kPa.
pub const P0_KPA: f64 = 101.325;
const T0_K: f64 = 288.15;
const LAPSE_K_PER_M: f64 = 0.0065;

/// Standard pressure at `alt_m` metres, kPa.
pub fn standard_pressure_kpa(alt_m: f64) -> f64 {
    P0_KPA * (1.0 - 2.25577e-5 * alt_m).max(0.0).powf(5.25588)
}

/// Standard temperature at `alt_m`, °C.
pub fn standard_temp_c(alt_m: f64) -> f64 {
    T0_K - LAPSE_K_PER_M * alt_m - 273.15
}

/// Air density relative to sea level standard (1.0 = 1.225 kg/m³), from
/// pressure (kPa) and temperature (°C): ideal gas, dry air.
pub fn density_ratio(p_kpa: f64, t_c: f64) -> f64 {
    (p_kpa / P0_KPA) * (T0_K / (t_c + 273.15))
}

/// A place from the Open-Meteo lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub name: String,
    /// State/province and country, for telling places apart.
    pub region: String,
    pub latitude: f64,
    pub longitude: f64,
    pub elevation_m: f64,
}

impl Place {
    pub fn label(&self) -> String {
        if self.region.is_empty() { self.name.clone() } else { format!("{}, {}", self.name, self.region) }
    }
}

/// Current weather at a place (Open-Meteo), with its time (ISO 8601).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Weather {
    pub time: String,
    pub surface_pressure_hpa: f64,
    pub temperature_c: f64,
}

/// The atmosphere block for the advisor: standard values for the altitude,
/// and the measured ones when there are any.
pub fn to_json(alt_m: Option<f64>, weather: Option<&Weather>) -> Value {
    let Some(alt) = alt_m else { return Value::String("altitude not set".into()) };
    let p = standard_pressure_kpa(alt);
    let t = standard_temp_c(alt);
    let mut v = serde_json::json!({
        "altitude_m": alt.round(),
        "standard_pressure_kpa": (p * 10.0).round() / 10.0,
        "standard_air_density_pct_of_sea_level": (density_ratio(p, t) * 1000.0).round() / 10.0,
    });
    if let Some(w) = weather {
        let pk = w.surface_pressure_hpa / 10.0;
        v["measured"] = serde_json::json!({
            "when": w.time,
            "surface_pressure_kpa": (pk * 10.0).round() / 10.0,
            "temperature_c": w.temperature_c,
            "air_density_pct_of_sea_level": (density_ratio(pk, w.temperature_c) * 1000.0).round() / 10.0,
        });
    }
    v
}

const GEOCODE_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";

fn get(url: &str) -> Result<Value, AiError> {
    let host = url.split('/').nth(2).unwrap_or_default().to_string();
    let cfg =
        crate::config::AiConfig { base_url: format!("https://{host}"), timeout_s: 15, ..Default::default() };
    let resp = crate::client::agent(&cfg).get(url).call();
    let mut resp = resp.map_err(|e| AiError::Network { host: host.clone(), message: e.to_string() })?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| AiError::Network { host: host.clone(), message: e.to_string() })?;
    if status != 200 {
        return Err(AiError::Http { host, status, message: body.chars().take(200).collect() });
    }
    serde_json::from_str(&body).map_err(|e| AiError::BadReply { host, message: e.to_string() })
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Places called `town` (blocking; on the user's click only).
pub fn lookup(town: &str) -> Result<Vec<Place>, AiError> {
    let v = get(&format!("{GEOCODE_URL}?name={}&count=8&language=en&format=json", encode(town.trim())))?;
    Ok(parse_places(&v))
}

pub fn parse_places(v: &Value) -> Vec<Place> {
    let Some(list) = v.get("results").and_then(Value::as_array) else { return Vec::new() };
    list.iter()
        .filter_map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            let region = [s("admin1"), s("country")]
                .into_iter()
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            Some(Place {
                name: s("name"),
                region,
                latitude: r.get("latitude")?.as_f64()?,
                longitude: r.get("longitude")?.as_f64()?,
                elevation_m: r.get("elevation")?.as_f64()?,
            })
        })
        .collect()
}

/// Current surface pressure and temperature at a place (blocking; on the
/// user's click only). Coordinates are rounded to 0.01°.
pub fn current(place: &Place) -> Result<Weather, AiError> {
    let v = get(&format!(
        "{FORECAST_URL}?latitude={:.2}&longitude={:.2}&current=surface_pressure,temperature_2m",
        place.latitude, place.longitude
    ))?;
    parse_weather(&v)
        .ok_or(AiError::BadReply { host: "api.open-meteo.com".into(), message: "no current values".into() })
}

pub fn parse_weather(v: &Value) -> Option<Weather> {
    let c = v.get("current")?;
    Some(Weather {
        time: c.get("time")?.as_str()?.to_string(),
        surface_pressure_hpa: c.get("surface_pressure")?.as_f64()?,
        temperature_c: c.get("temperature_2m")?.as_f64()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_atmosphere() {
        assert!((standard_pressure_kpa(0.0) - 101.325).abs() < 1e-9);
        // ISA tables: 1500 m 84.56 kPa, 3000 m 70.12 kPa.
        assert!((standard_pressure_kpa(1500.0) - 84.56).abs() < 0.05);
        assert!((standard_pressure_kpa(3000.0) - 70.12).abs() < 0.05);
        assert!((density_ratio(P0_KPA, 15.0) - 1.0).abs() < 1e-9);
        // ISA density at 1500 m is about 86% of sea level.
        let d = density_ratio(standard_pressure_kpa(1500.0), standard_temp_c(1500.0));
        assert!((d - 0.8633).abs() < 0.002, "{d}");
    }

    #[test]
    fn open_meteo_answers_as_seen_2026_10_19() {
        let g: Value = serde_json::from_str(r#"{"results":[{"id":5419384,"name":"Denver","latitude":39.73915,"longitude":-104.9847,"elevation":1609.0,"country":"United States","admin1":"Colorado"}]}"#).unwrap();
        let p = parse_places(&g);
        assert_eq!(p[0].label(), "Denver, Colorado, United States");
        assert_eq!(p[0].elevation_m, 1609.0);
        let w: Value = serde_json::from_str(r#"{"current":{"time":"2026-10-19T16:30","interval":900,"surface_pressure":841.6,"pressure_msl":1016.6,"temperature_2m":13.2}}"#).unwrap();
        let w = parse_weather(&w).unwrap();
        assert_eq!(w.surface_pressure_hpa, 841.6);
        let j = to_json(Some(1609.0), Some(&w));
        assert_eq!(j["measured"]["surface_pressure_kpa"], 84.2);
        assert!(j["standard_pressure_kpa"].as_f64().unwrap() > 83.0);
        assert_eq!(to_json(None, None), "altitude not set");
        assert_eq!(encode("São Paulo"), "S%C3%A3o+Paulo");
    }
}
