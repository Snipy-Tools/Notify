use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;

pub fn dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("notify");
    fs::create_dir_all(&dir).ok();
    dir
}

// Journal und Einstellungen liegen im Roaming-Ordner
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("notify");
    fs::create_dir_all(&dir).ok();
    dir
}

fn random() -> u64 {
    RandomState::new().build_hasher().finish()
}

pub fn token() -> String {
    let path = dir().join("token");
    if let Ok(saved) = fs::read_to_string(&path)
        && saved.trim().len() == 32
    {
        return saved.trim().to_string();
    }
    let token = format!("{:016x}{:016x}", random(), random());
    fs::write(&path, &token).ok();
    token
}

pub fn load_pos() -> Option<(i32, i32)> {
    let saved = fs::read_to_string(dir().join("pos")).ok()?;
    let (x, y) = saved.trim().split_once(',')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

pub fn save_pos(x: i32, y: i32) {
    fs::write(dir().join("pos"), format!("{x},{y}")).ok();
}
