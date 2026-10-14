use crate::{log, system};

const TITLE: &str = "Notify – Update";
#[cfg(any(windows, target_os = "macos"))]
const API: &str = "https://api.github.com/repos/Snipy-Tools/Notify/releases/latest";
#[cfg(windows)]
const ASSET: &str = "notify-Windows-Setup.exe";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ASSET: &str = "notify-macOS-arm64.dmg";
#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
const ASSET: &str = "notify-macOS-x86_64.dmg";
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

#[cfg(not(any(windows, target_os = "macos")))]
fn run() -> Result<(), String> {
    Err("Neue Versionen gibt es unter https://github.com/Snipy-Tools/Notify/releases/latest".to_string())
}

#[cfg(any(windows, target_os = "macos"))]
fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let mut command = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    command.args(["--silent", "--show-error", "--fail", "--location", "--max-time", "300"]).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let out = command.output().map_err(|e| format!("curl nicht gestartet: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!("Download fehlgeschlagen: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(any(windows, target_os = "macos"))]
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
    install(&path)
}

/// Der Installer schliesst Notify selbst und bietet am Ende den Neustart an
#[cfg(windows)]
fn install(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new(path).spawn().map_err(|e| format!("Installer nicht gestartet: {e}"))?;
    Ok(())
}

/// Ersetzt Notify.app durch die App aus dem dmg und startet sie neu, sobald diese Instanz beendet ist
#[cfg(target_os = "macos")]
fn install(dmg: &std::path::Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("Pfad der App nicht ermittelbar: {e}"))?;
    let app = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .ok_or("Notify läuft nicht aus einer Notify.app, bitte das dmg manuell installieren")?;
    const SCRIPT: &str = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.5; done
mnt=$(mktemp -d)
if hdiutil attach -nobrowse -quiet -mountpoint "$mnt" "$2"; then
  rm -rf "$3" && cp -R "$mnt/Notify.app" "$3"
  hdiutil detach -quiet "$mnt"
fi
rm -f "$2"
open "$3""#;
    std::process::Command::new("sh")
        .args(["-c", SCRIPT, "sh", &std::process::id().to_string()])
        .arg(dmg)
        .arg(app)
        .spawn()
        .map_err(|e| format!("Update nicht gestartet: {e}"))?;
    std::process::exit(0);
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
