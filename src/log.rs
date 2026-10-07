use crate::store;
use std::fs::{self, OpenOptions};
use std::io::Write;

const MAX_SIZE: u64 = 512 * 1024;

fn write(level: &str, msg: &str) {
    let line = format!("{} {level} {msg}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    if cfg!(test) {
        eprint!("{line}");
        return;
    }

    let path = store::dir().join("notify.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_SIZE) {
        fs::rename(&path, path.with_extension("log.old")).ok();
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        file.write_all(line.as_bytes()).ok();
    }
}

pub fn info(msg: &str) {
    write("INFO", msg);
}

pub fn warn(msg: &str) {
    write("WARN", msg);
}

pub fn error(msg: &str) {
    write("ERROR", msg);
}
