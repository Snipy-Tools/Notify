use crate::{log, system};

const TITLE: &str = "Notify – Update";
#[cfg(windows)]
const API: &str = "https://api.github.com/repos/Snipy-Tools/Notify/releases/latest";
#[cfg(windows)]
const ASSET: &str = "notify-Windows-Setup.exe";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Versionsnummer "v0.1.8" oder "0.1.8" als Zahlen
fn parse_version(text: &str) -> Option<Vec<u32>> {
    text.trim().trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect()
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Sucht im Hintergrund nach einer neuen Version und bietet die Installation an
pub fn check() {
    std::thread::spawn(|| {
        if let Err(e) = run() {
            log::warn(&format!("Update fehlgeschlagen: {e}"));
            system::show_error(TITLE, &e);
        }
    });
}

#[cfg(not(windows))]
fn run() -> Result<(), String> {
    Err("Neue Versionen gibt es unter https://github.com/Snipy-Tools/Notify/releases/latest".to_string())
}

#[cfg(windows)]
fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("curl.exe")
        .args(["--silent", "--show-error", "--fail", "--location", "--max-time", "300"])
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("curl.exe nicht gestartet: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!("Download fehlgeschlagen: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    let body = curl(&[API])?;
    let release: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("Antwort unlesbar: {e}"))?;
    let tag = release["tag_name"].as_str().ok_or("Keine Version gefunden")?;
    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(tag, current) {
        system::show_info(TITLE, &format!("Notify {current} ist aktuell."));
        return Ok(());
    }
    let url = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"] == ASSET)
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or_else(|| format!("{tag} enthält keinen Installer ({ASSET})"))?;
    if !system::ask(TITLE, &format!("Version {tag} ist verfügbar (installiert: {current}).\n\nJetzt herunterladen und installieren?")) {
        return Ok(());
    }
    let path = std::env::temp_dir().join(ASSET);
    curl(&["--output", &path.to_string_lossy(), url])?;
    // Der Installer schliesst Notify selbst und bietet am Ende den Neustart an
    std::process::Command::new(&path).spawn().map_err(|e| format!("Installer nicht gestartet: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.1.9", "0.1.8"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(!is_newer("v0.1.8", "0.1.8"));
        assert!(!is_newer("v0.1.7", "0.1.8"));
        assert!(!is_newer("Nightly", "0.1.8"));
    }
}
