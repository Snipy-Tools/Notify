use crate::log;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const IDLE_CACHE: Duration = Duration::from_secs(5);

static LOCK: OnceLock<File> = OnceLock::new();
static IDLE: Mutex<Option<(Instant, Duration)>> = Mutex::new(None);

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn acquire_single_instance() -> bool {
    let Ok(file) = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(crate::store::dir().join("notify.lock"))
    else {
        return true;
    };
    if file.try_lock().is_err() {
        return false;
    }
    LOCK.set(file).ok();
    true
}

fn output(command: &mut Command) -> Option<String> {
    let out = command.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn picked(command: &mut Command) -> Option<PathBuf> {
    output(command).filter(|s| !s.is_empty()).map(PathBuf::from)
}

pub fn pick_folder(_owner: isize, title: &str) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let script = format!("POSIX path of (choose folder with prompt {title:?})");
        picked(Command::new("osascript").args(["-e", &script]))
    } else {
        picked(Command::new("zenity").args(["--file-selection", "--directory", &format!("--title={title}")]))
    }
}

pub fn pick_docx(_owner: isize, title: &str) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let script = format!("POSIX path of (choose file with prompt {title:?} of type {{\"docx\"}})");
        picked(Command::new("osascript").args(["-e", &script]))
    } else {
        picked(Command::new("zenity").args([
            "--file-selection",
            &format!("--title={title}"),
            "--file-filter=Word-Vorlage | *.docx",
        ]))
    }
}

pub fn idle_time() -> Duration {
    let mut cache = IDLE.lock().unwrap();
    if let Some((at, idle)) = *cache
        && at.elapsed() < IDLE_CACHE
    {
        return idle;
    }
    let idle = read_idle().unwrap_or(Duration::ZERO);
    *cache = Some((Instant::now(), idle));
    idle
}

fn read_idle() -> Option<Duration> {
    if cfg!(target_os = "macos") {
        let text = output(Command::new("ioreg").args(["-c", "IOHIDSystem"]))?;
        let line = text.lines().find(|l| l.contains("HIDIdleTime"))?;
        let nanos: u64 = line.rsplit('=').next()?.trim().parse().ok()?;
        Some(Duration::from_nanos(nanos))
    } else {
        let millis: u64 = output(&mut Command::new("xprintidle"))?.parse().ok()?;
        Some(Duration::from_millis(millis))
    }
}

pub fn documents_dir() -> Option<PathBuf> {
    home().map(|h| h.join("Documents"))
}

fn autostart_path() -> Option<PathBuf> {
    let home = home()?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/LaunchAgents/ch.notify.arbeitsjournal.plist")
    } else {
        let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
        config.join("autostart/notify.desktop")
    })
}

pub fn autostart_enabled() -> bool {
    autostart_path().is_some_and(|p| p.exists())
}

pub fn set_autostart(on: bool) -> Result<(), String> {
    let path = autostart_path().ok_or("Das Benutzerverzeichnis ist nicht ermittelbar")?;
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("Der Autostart konnte nicht entfernt werden: {e}"))
            }
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| format!("Pfad der App nicht ermittelbar: {e}"))?;
    let exe = exe.display();
    let content = if cfg!(target_os = "macos") {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n<key>Label</key><string>ch.notify.arbeitsjournal</string>\n<key>ProgramArguments</key><array><string>{exe}</string></array>\n<key>RunAtLoad</key><true/>\n</dict>\n</plist>\n"
        )
    } else {
        format!("[Desktop Entry]\nType=Application\nName=Notify\nExec=\"{exe}\"\nX-GNOME-Autostart-enabled=true\n")
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Der Autostart konnte nicht gesetzt werden: {e}"))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("Der Autostart konnte nicht gesetzt werden: {e}"))
}

fn open_path(path: &Path) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(e) = Command::new(opener).arg(path).spawn() {
        log::warn(&format!("{opener} nicht gestartet: {e}"));
    }
}

pub fn open_folder(path: &Path) {
    open_path(path);
}

pub fn reveal_in_explorer(path: &Path) {
    if cfg!(target_os = "macos") {
        if let Err(e) = Command::new("open").arg("-R").arg(path).spawn() {
            log::warn(&format!("open nicht gestartet: {e}"));
        }
    } else {
        open_path(path.parent().unwrap_or(path));
    }
}

pub fn show_error(title: &str, text: &str) {
    let (title, text) = (title.to_string(), text.to_string());
    std::thread::spawn(move || {
        let shown = if cfg!(target_os = "macos") {
            let script = format!("display alert {title:?} message {text:?} as critical");
            Command::new("osascript").args(["-e", &script]).status().is_ok()
        } else {
            Command::new("zenity")
                .args(["--error", &format!("--title={title}"), &format!("--text={text}")])
                .status()
                .is_ok()
        };
        if !shown {
            eprintln!("{title}: {text}");
        }
    });
}

pub fn attach_parent_console() {}

pub fn console_line(text: &str) {
    println!("{text}");
}

pub fn session_away() -> bool {
    false
}

pub fn start_session_watcher() {}
