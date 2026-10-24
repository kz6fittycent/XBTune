// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows: put the tach icon into xbtune.exe (Explorer, Start, taskbar).

fn main() {
    println!("cargo:rerun-if-changed=assets/xbtune.rc");
    println!("cargo:rerun-if-changed=assets/xbtune.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let r = embed_resource::compile("assets/xbtune.rc", embed_resource::NONE);
    // Release builds (MSVC, in CI) must carry the icon; a cross type-check
    // without a resource compiler may go without.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        r.manifest_required().expect("compiling the Windows resources");
    } else if let Err(e) = r.manifest_optional() {
        println!("cargo:warning=no icon in the .exe: {e}");
    }
}
