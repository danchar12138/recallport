//! A small log file next to the data. The Windows build has no console, so this is how problems get seen.
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    sync::{Mutex, OnceLock},
};

static FILE: OnceLock<Mutex<File>> = OnceLock::new();
const LIMIT: u64 = 2 << 20;

pub fn init(dir: &Path) {
    let path = dir.join("recallport.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > LIMIT) {
        let _ = fs::rename(&path, dir.join("recallport.log.old"));
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = FILE.set(Mutex::new(file));
    }
}

pub fn write(message: &str) {
    let line = format!(
        "{} {message}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    let _ = std::io::stderr().write_all(line.as_bytes());
    if let Some(file) = FILE.get() {
        let mut file = file.lock().unwrap_or_else(|e| e.into_inner());
        let _ = file.write_all(line.as_bytes());
    }
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => { $crate::log::write(&format!($($t)*)) };
}
