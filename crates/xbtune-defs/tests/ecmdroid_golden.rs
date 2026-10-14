// SPDX-License-Identifier: GPL-3.0-or-later
//! Golden tests ported from EcmDroid's own tests
//! (`app/src/androidTest/java/org/ecmdroid/TestVariableProvider.java`,
//! `TestBitSetProvider.java`, `TestECM.java`, commit 2f17d29f), run against
//! EcmDroid's fixtures, plus consistency checks over every definition.

use xbtune_defs::{Catalog, Definition, EeKind, ImageView, RtKind, Shape, Value};

const BUEIB_EEPROM: &[u8] = include_bytes!("../../../tests/fixtures/BUEIB.eeprom");
const RT_BUEIB242: &[u8] = include_bytes!("../../../tests/fixtures/RT_BUEIB242.bin");

fn def(id: &str) -> std::sync::Arc<Definition> {
    let c = Catalog::builtin().unwrap();
    let ecm = c.lookup(id).unwrap().clone();
    c.definition(&ecm).unwrap().clone()
}

/// EcmDroid's BUEIB test image has no page 0 (1206 bytes); pad to xsize.
fn bueib_image() -> Vec<u8> {
    let mut v = BUEIB_EEPROM.to_vec();
    v.resize(1210, 0);
    v
}

#[test]
fn rt_variable_names_bue2d() {
    let d = def("BUE2D");
    assert_eq!(d.runtime.len(), 111);
    assert_eq!(d.runtime.values().filter(|v| v.kind == RtKind::Scalar).count(), 79);
    assert_eq!(d.runtime.values().filter(|v| v.kind == RtKind::Bitfield).count(), 32);
    for n in ["RPM", "TPD", "CLT", "O2 ADC", "ETS1 ADC", "EGO1 Corr.", "Flags0", "CDiag7", "DOut2 Feedb."] {
        assert!(d.runtime.contains_key(n), "{n}");
    }
}

#[test]
fn rt_variable_pw2() {
    let v = def("BUE2D").runtime["pw2"].clone();
    assert_eq!(v.kind, RtKind::Scalar);
    assert_eq!((v.size, v.offset), (2, 23));
    assert_eq!(v.names.units.as_deref(), Some("Milliseconds"));
    assert!((v.scale - 0.00133).abs() < 1e-6);
    assert_eq!(v.translate, 0.0);
    assert_eq!(v.label(), "Fuel Pulsewidth Rear");
    assert_eq!(v.names.decimals, Some(2));
    assert_eq!(v.low, Some(0.0));
    // EcmDroid's test expects 0.1159 here because Variable.getHigh() scales
    // an already-scaled value. We keep the stored engineering value:
    // 65535 * 0.00133 = 87.16 ms.
    assert!((v.high.unwrap() - 87.16).abs() < 0.01);
}

#[test]
fn rt_parsing_afv() {
    let v = def("BUEIB").runtime["AFV"].clone();
    let val = v.value(RT_BUEIB242).unwrap();
    assert_eq!(xbtune_defs::format_number(val, v.names.decimals), "100.0");
    assert_eq!(v.names.units.as_deref().and_then(xbtune_defs::unit_symbol), Some("%"));
}

#[test]
fn all_rt_variables_decode() {
    let d = def("BUEIB");
    for v in d.runtime.values() {
        assert!(v.value(RT_BUEIB242).is_some(), "{}", v.name);
    }
}

#[test]
fn eeprom_scalars() {
    let d = def("BUEIB");
    let img = bueib_image();
    let view = ImageView { bytes: &img, page_zero: false };
    let num = |n: &str| match d.eeprom[n].decode(&view).unwrap() {
        Value::Number(x) => x,
        other => panic!("{n}: {other:?}"),
    };
    assert_eq!(num("Country_ID"), 77.0);
    assert_eq!(num("KMFG_Year"), 6.0);
    let serial = &d.eeprom["KMFG_Serial"];
    assert_eq!((serial.width, serial.size, serial.element_count()), (2, 2, 1));
    assert_eq!(serial.kind, EeKind::Value);
    assert_eq!(num("KMFG_Serial"), 204.0);
    // TestECM.testMfgDate: "6/8/06" = 2006, day-of-year KMFG_Day + 1 = 159.
    assert_eq!(num("KMFG_Day"), 158.0);
}

#[test]
fn page_zero_variables_need_page_zero() {
    let d = def("BUEIB");
    let img = bueib_image();
    let no_p0 = ImageView { bytes: &img, page_zero: false };
    assert_eq!(d.eeprom["KBaro"].decode(&no_p0), Err(xbtune_defs::DecodeError::NoPageZero));
    let with_p0 = ImageView { bytes: &img, page_zero: true };
    assert!(d.eeprom["KBaro"].decode(&with_p0).is_ok());
    assert_eq!(d.eeprom["KBaro"].start(&with_p0).unwrap(), 1208);
}

#[test]
fn axes() {
    let d = def("BUEIB");
    let img = bueib_image();
    let view = ImageView { bytes: &img, page_zero: false };
    let load = &d.eeprom["Tab_Fuel_Load_Ax"];
    assert_eq!((load.kind, load.size, load.width), (EeKind::Axis, 12, 1));
    assert_eq!(
        load.decode(&view).unwrap(),
        Value::List(vec![10., 15., 20., 30., 40., 50., 60., 80., 100., 125., 175., 255.])
    );
    let rpm = &d.eeprom["Tab_Fuel_RPM_Ax"];
    assert_eq!((rpm.size, rpm.width), (26, 2));
    assert_eq!(
        rpm.decode(&view).unwrap(),
        Value::List(vec![
            8000., 7000., 6000., 5000., 4000., 3400., 2900., 2400., 1900., 1350., 1000., 800., 0.
        ])
    );
}

#[test]
fn table_pairs() {
    let d = def("BUEIB");
    let img = bueib_image();
    let view = ImageView { bytes: &img, page_zero: false };
    let t = &d.eeprom["Tab_ABP_Conv"];
    assert_eq!(t.shape, Shape::Pairs { rows: 5 });
    assert_eq!(
        t.decode(&view).unwrap(),
        Value::Pairs(vec![(125., 100.), (127., 60.), (170., 80.), (213., 100.), (250., 120.)])
    );
}

#[test]
fn fuel_map_shape_and_values() {
    let d = def("BUEIB");
    let img = bueib_image();
    let view = ImageView { bytes: &img, page_zero: false };
    let m = &d.eeprom["Tab_Fuel_Front"];
    assert_eq!(m.shape, Shape::Grid { rows: 12, cols: 13, skip: 1 });
    assert_eq!(m.x_axis.as_deref(), Some("Tab_Fuel_RPM_Ax"));
    assert_eq!(m.y_axis.as_deref(), Some("Tab_Fuel_Load_Ax"));
    let Value::Grid(rows) = m.decode(&view).unwrap() else { panic!() };
    assert_eq!(rows.len(), 12);
    assert_eq!(rows[0], vec![22., 22., 22., 22., 30., 31., 34., 36., 38., 44., 64., 61., 75.]);
    assert_eq!(rows[11][2], 254.0);
    // Skip bytes (one per row) are zero in this image and are not values.
    let offs = m.element_offsets(&view).unwrap();
    assert_eq!(offs[13], 870 + 14);
}

#[test]
fn bitsets() {
    let d = def("BUEIB");
    let cdiag0 = &d.runtime["CDiag0"];
    assert_eq!(cdiag0.offset, 67);
    assert_eq!(cdiag0.kind, RtKind::Bitfield);
    let bits = cdiag0.bits.as_ref().unwrap();
    for (i, b) in bits.bits.iter().enumerate() {
        assert_eq!(b.nr as usize, i);
    }
    // TestBitSetProvider.testFlags1: only bit 7 set.
    let flags1 = &d.runtime["Flags1"];
    assert_eq!(flags1.raw(RT_BUEIB242).unwrap() as u8, 0x80);
}

#[test]
fn current_trouble_code_from_runtime() {
    // TestECM.testErrorParsing: one current code, "21", in RT_BUEIB242.
    let d = def("BUEIB");
    let mut active = Vec::new();
    for i in 0.. {
        let Some(v) = d.runtime.get(&format!("CDiag{i}")) else { break };
        let byte = v.raw(RT_BUEIB242).unwrap() as u8;
        for b in &v.bits.as_ref().unwrap().bits {
            if byte & (1 << b.nr) != 0 {
                active.push(b.dtc.clone().unwrap_or_default());
            }
        }
    }
    assert_eq!(active, vec!["21".to_string()]);
}

#[test]
fn every_definition_is_consistent() {
    let c = Catalog::builtin().unwrap();
    for ecm in c.iter().filter(|e| e.family.in_scope()) {
        let d = c.definition(ecm).unwrap();
        let img = vec![0u8; ecm.xsize];
        let view = ImageView { bytes: &img, page_zero: true };
        for v in d.eeprom.values() {
            if !matches!(v.shape, Shape::Invalid(_)) {
                v.decode(&view).unwrap_or_else(|e| panic!("{} {}: {e}", ecm.id, v.name));
            }
        }
        // Every main fuel and spark map must be understood.
        for name in ["Tab_Fuel_Front", "Tab_Fuel_Rear", "Tab_Spark_Front", "Tab_Spark_Rear"] {
            if let Some(m) = d.eeprom.get(name) {
                assert!(matches!(m.shape, Shape::Grid { .. }), "{} {name}: {:?}", ecm.id, m.shape);
            }
        }
        let frame = vec![0u8; ecm.runtime_len];
        for v in d.runtime.values() {
            assert!(v.value(&frame).is_some(), "{} {}", ecm.id, v.name);
        }
    }
}

#[test]
fn known_data_issues_are_reported() {
    let c = Catalog::builtin().unwrap();
    let bueod = c.definition(c.lookup("BUEOD").unwrap()).unwrap();
    assert_eq!(bueod.conflicts, vec!["KVS_RPM_Revs".to_string()]);
    assert!(!bueod.eeprom.contains_key("KVS_RPM_Revs"));
    let bue1d = c.definition(c.lookup("BUE1D").unwrap()).unwrap();
    assert!(matches!(bue1d.eeprom["Tab_Idle_Spark"].shape, Shape::Invalid(_)));
}
