//! Embeds `assets/recallport.ico` as the Windows executable icon.
//!
//! A `.res` file is written directly (icons may hold PNG data), so no resource compiler such as
//! `rc.exe` or `windres` is needed to build.

use std::{env, fs, path::PathBuf};

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;

fn main() {
    println!("cargo:rerun-if-changed=assets/recallport.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = fs::read("assets/recallport.ico").expect("assets/recallport.ico is missing");
    let res = build_res(&ico);
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("recallport-icon.res");
    fs::write(&out, res).unwrap();
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// One resource record: 32-byte header followed by the data, padded to four bytes.
fn record(out: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(kind.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(id.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // data version
    out.extend(0x1030u16.to_le_bytes()); // MOVEABLE | PURE | DISCARDABLE
    out.extend(0x0409u16.to_le_bytes()); // en-US
    out.extend(0u32.to_le_bytes()); // version
    out.extend(0u32.to_le_bytes()); // characteristics
    out.extend(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn build_res(ico: &[u8]) -> Vec<u8> {
    assert!(ico.len() >= 6 && u16_at(ico, 2) == 1, "not an .ico file");
    let count = u16_at(ico, 4) as usize;
    let mut res = Vec::new();
    record(&mut res, 0, 0, &[]); // the mandatory empty first record
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + i * 16;
        let size = u32_at(ico, e + 8) as usize;
        let offset = u32_at(ico, e + 12) as usize;
        let id = (i + 1) as u16;
        record(&mut res, RT_ICON, id, &ico[offset..offset + size]);
        group.extend(&ico[e..e + 12]); // width, height, colors, reserved, planes, bit count, size
        group.extend(id.to_le_bytes());
    }
    record(&mut res, RT_GROUP_ICON, 1, &group);
    res
}
