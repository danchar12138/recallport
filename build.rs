//! Embeds the application icon (`assets/recallport.ico`) and version information into the
//! Windows executable, so Explorer, the taskbar and Settings -> Startup show "RecallPort".
//!
//! A `.res` file is written directly (icons may hold PNG data), so no resource compiler such as
//! `rc.exe` or `windres` is needed to build.

use std::{env, fs, path::PathBuf};

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;

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
    record(&mut res, RT_VERSION, 1, &version_info());
    res
}

fn pad4(b: &mut Vec<u8>) {
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
}

fn wide(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// One VS_VERSIONINFO node: lengths, key, value, then children, each aligned to four bytes.
fn node(key: &str, kind: u16, value: &[u8], value_len: u16, children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0, 0];
    b.extend(value_len.to_le_bytes());
    b.extend(kind.to_le_bytes());
    b.extend(wide(key));
    pad4(&mut b);
    b.extend(value);
    for child in children {
        pad4(&mut b);
        b.extend(child);
    }
    let len = b.len() as u16;
    b[..2].copy_from_slice(&len.to_le_bytes());
    b
}

fn version_info() -> Vec<u8> {
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor, patch) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    let (ms, ls) = ((major << 16) | minor, patch << 16);
    let mut fixed = Vec::new();
    for v in [
        0xFEEF04BD,
        0x0001_0000,
        ms,
        ls,
        ms,
        ls,
        0x3F,
        0,
        0x0004_0004,
        1,
        0,
        0,
        0,
    ] {
        fixed.extend(u32::to_le_bytes(v));
    }
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", "RecallPort"),
        ("FileDescription", "RecallPort"),
        ("FileVersion", version.as_str()),
        ("InternalName", "RecallPort"),
        ("LegalCopyright", "MIT License"),
        ("OriginalFilename", "RecallPort.exe"),
        ("ProductName", "RecallPort"),
        ("ProductVersion", version.as_str()),
    ]
    .iter()
    .map(|(k, v)| {
        let value = wide(v);
        node(k, 1, &value, (value.len() / 2) as u16, &[])
    })
    .collect();
    let table = node("040904B0", 1, &[], 0, &strings);
    let string_info = node("StringFileInfo", 1, &[], 0, &[table]);
    let translation = node("Translation", 0, &[0x09, 0x04, 0xB0, 0x04], 4, &[]);
    let var_info = node("VarFileInfo", 1, &[], 0, &[translation]);
    node("VS_VERSION_INFO", 0, &fixed, 52, &[string_info, var_info])
}
