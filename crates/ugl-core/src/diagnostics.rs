//! Registro sencillo en archivo (sin dependencias). Rota al superar ~1 MB.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::domain::unix_now;

static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();
const MAX_LOG_BYTES: u64 = 1_000_000;

pub fn init(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let too_big = fs::metadata(path)
        .map(|m| m.len() > MAX_LOG_BYTES)
        .unwrap_or(false);
    if too_big {
        let old = path.with_extension("log.old");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(path, &old);
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let _ = LOG_FILE.set(Mutex::new(file));
    Ok(())
}

fn write(level: &str, message: &str) {
    let line = format!("[{}] {level}: {message}\n", unix_now());
    #[cfg(debug_assertions)]
    eprint!("{line}");
    if let Some(file) = LOG_FILE.get() {
        if let Ok(mut file) = file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

pub fn info(message: &str) {
    write("INFO", message);
}

pub fn warn(message: &str) {
    write("WARN", message);
}

pub fn error(message: &str) {
    write("ERROR", message);
}
