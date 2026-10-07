use crate::log;
use std::ffi::{OsString, c_void};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::System::RemoteDesktop::WTSRegisterSessionNotification;
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows_sys::Win32::UI::Shell::{FOLDERID_Documents, SHGetKnownFolderPath};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MB_ICONERROR, MB_OK, MSG, MessageBoxW,
    RegisterClassW, WNDCLASSW,
};

const WM_POWERBROADCAST: u32 = 0x0218;
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const PBT_APMSUSPEND: usize = 0x04;
const PBT_APMRESUMESUSPEND: usize = 0x07;
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;
const WTS_CONSOLE_CONNECT: usize = 1;
const WTS_CONSOLE_DISCONNECT: usize = 2;
const WTS_REMOTE_CONNECT: usize = 3;
const WTS_REMOTE_DISCONNECT: usize = 4;
const WTS_SESSION_LOCK: usize = 7;
const WTS_SESSION_UNLOCK: usize = 8;
const NOTIFY_FOR_THIS_SESSION: u32 = 0;

static LOCKED: AtomicBool = AtomicBool::new(false);
static SUSPENDED: AtomicBool = AtomicBool::new(false);

fn idle_from_ticks(now: u32, last_input: u32) -> Duration {
    // Der Tick-Zähler läuft nach etwa 49 Tagen über, wrapping_sub bleibt dabei richtig
    Duration::from_millis(u64::from(now.wrapping_sub(last_input)))
}

/// Zeit seit der letzten Tastatur- oder Mauseingabe
pub fn idle_time() -> Duration {
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: `info` ist initialisiert und `cbSize` gesetzt
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return Duration::ZERO;
    }
    // SAFETY: GetTickCount hat keine Voraussetzungen
    idle_from_ticks(unsafe { GetTickCount() }, info.dwTime)
}

/// Der Ordner "Dokumente" des Benutzers (auch wenn er umgeleitet ist, z. B. nach OneDrive)
pub fn documents_dir() -> Option<PathBuf> {
    let mut raw: *mut u16 = null_mut();
    // SAFETY: `raw` ist ein gültiger Zeiger auf einen Zeiger; bei Erfolg gehört der Speicher uns und wird mit CoTaskMemFree frei gegeben
    let path = unsafe {
        let hr = SHGetKnownFolderPath(&FOLDERID_Documents, 0, null_mut(), &mut raw);
        let path = if hr >= 0 && !raw.is_null() {
            let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
            Some(PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(raw, len))))
        } else {
            None
        };
        CoTaskMemFree(raw as *const c_void);
        path
    };
    path.or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Documents")))
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const AUTOSTART_NAME: &str = "Notify";
const ERROR_FILE_NOT_FOUND: u32 = 2;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_run_key(access: u32) -> Result<HKEY, String> {
    let mut key: HKEY = null_mut();
    // SAFETY: der Unterschlüssel ist nullterminiert und `key` ein gültiger Zeiger
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), 0, access, &mut key) };
    if status == 0 { Ok(key) } else { Err(format!("Die Registry ist nicht zugänglich (Fehler {status})")) }
}

fn run_value(name: &str) -> Option<String> {
    let key = open_run_key(KEY_READ).ok()?;
    let name = wide(name);
    let (mut kind, mut size) = (0u32, 0u32);
    // SAFETY: Schlüssel und Namen sind gültig, der Puffer ist so gross wie `bytes` angibt
    let value = unsafe {
        let status = RegQueryValueExW(key, name.as_ptr(), std::ptr::null(), &mut kind, null_mut(), &mut size);
        if status != 0 || kind != REG_SZ || size == 0 {
            None
        } else {
            let mut buf = vec![0u16; (size as usize).div_ceil(2)];
            let mut bytes = (buf.len() * 2) as u32;
            let status = RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null(),
                null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            );
            (status == 0).then(|| String::from_utf16_lossy(&buf).trim_end_matches('\0').to_string())
        }
    };
    // SAFETY: `key` wurde oben geöffnet
    unsafe { RegCloseKey(key) };
    value
}

fn set_run_value(name: &str, command: &str) -> Result<(), String> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let (name, data) = (wide(name), wide(command));
    // SAFETY: `data` ist nullterminiert, die Länge in Bytes stimmt
    let status = unsafe {
        let status = RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32);
        RegCloseKey(key);
        status
    };
    if status == 0 { Ok(()) } else { Err(format!("Der Autostart konnte nicht gesetzt werden (Fehler {status})")) }
}

fn delete_run_value(name: &str) -> Result<(), String> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let name = wide(name);
    // SAFETY: `name` ist nullterminiert
    let status = unsafe {
        let status = RegDeleteValueW(key, name.as_ptr());
        RegCloseKey(key);
        status
    };
    // Ein Wert, den es nicht gibt, ist schon "aus"
    if status == 0 || status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("Der Autostart konnte nicht entfernt werden (Fehler {status})"))
    }
}

pub fn autostart_command(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// Läuft die App beim Anmelden? (Eintrag in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`)
pub fn autostart_enabled() -> bool {
    run_value(AUTOSTART_NAME).is_some()
}

pub fn set_autostart(on: bool) -> Result<(), String> {
    if on {
        let exe = std::env::current_exe().map_err(|e| format!("Pfad der App nicht ermittelbar: {e}"))?;
        set_run_value(AUTOSTART_NAME, &autostart_command(&exe))
    } else {
        delete_run_value(AUTOSTART_NAME)
    }
}

/// Öffnet einen Ordner im Explorer
pub fn open_folder(path: &Path) {
    if let Err(e) = Command::new("explorer.exe").arg(path).spawn() {
        log::warn(&format!("Explorer nicht gestartet: {e}"));
    }
}

/// Öffnet den Explorer und markiert die Datei
pub fn reveal_in_explorer(path: &Path) {
    let result = Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn();
    if let Err(e) = result {
        log::warn(&format!("Explorer nicht gestartet: {e}"));
    }
}

/// Zeigt eine Fehlermeldung, ohne die Oberfläche zu blockieren
pub fn show_error(title: &str, text: &str) {
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let (title, text) = (wide(title), wide(text));
    std::thread::spawn(move || {
        // SAFETY: beide Puffer sind nullterminiert und leben bis zum Ende des Aufrufs
        unsafe { MessageBoxW(null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
    });
}

/// Hängt die App an die Konsole des Aufrufers (nötig, weil sie sonst ohne Konsole startet)
pub fn attach_parent_console() {
    // SAFETY: keine Zeiger, schlägt harmlos fehl, wenn es keine Konsole gibt
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

pub fn console_line(text: &str) {
    match OpenOptions::new().write(true).open("CONOUT$") {
        Ok(mut console) => {
            writeln!(console, "{text}").ok();
        }
        Err(_) => eprintln!("{text}"),
    }
}

/// Bildschirm gesperrt, Sitzung getrennt oder Rechner im Standby
pub fn session_away() -> bool {
    LOCKED.load(Ordering::Relaxed) || SUSPENDED.load(Ordering::Relaxed)
}

fn handle_message(msg: u32, wparam: usize) {
    match (msg, wparam) {
        (WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK | WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT) => {
            LOCKED.store(true, Ordering::Relaxed);
        }
        (WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK | WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT) => {
            LOCKED.store(false, Ordering::Relaxed);
        }
        (WM_POWERBROADCAST, PBT_APMSUSPEND) => SUSPENDED.store(true, Ordering::Relaxed),
        (WM_POWERBROADCAST, PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC) => {
            SUSPENDED.store(false, Ordering::Relaxed);
        }
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    handle_message(msg, wparam);
    // SAFETY: die Argumente kommen unverändert vom System
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Startet einen Thread mit unsichtbarem Fenster, das Sperre, Entsperren, Standby und Aufwachen meldet.
/// Schlägt das fehl, wird es geloggt und die Leerlaufzeit allein entscheidet.
pub fn start_session_watcher() {
    let spawned = std::thread::Builder::new().name("session-watcher".into()).spawn(|| {
        if let Err(e) = watch() {
            log::warn(&format!("Sitzungsüberwachung nicht verfügbar: {e}"));
        }
    });
    if let Err(e) = spawned {
        log::warn(&format!("Sitzungsüberwachung nicht gestartet: {e}"));
    }
}

fn watch() -> Result<(), String> {
    let class: Vec<u16> = "NotifySessionWatcher\0".encode_utf16().collect();
    // SAFETY: alle Zeiger zeigen auf lebende Daten, `class` überlebt den Thread (Endlosschleife unten)
    unsafe {
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = GetModuleHandleW(std::ptr::null());
        wc.lpszClassName = class.as_ptr();
        if RegisterClassW(&wc) == 0 {
            return Err("Fensterklasse nicht registriert".to_string());
        }

        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            wc.hInstance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return Err("Fenster nicht erstellt".to_string());
        }
        if WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) == 0 {
            return Err("Sitzungsbenachrichtigung nicht registriert".to_string());
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_ticks_wrap_around() {
        assert_eq!(idle_from_ticks(5_000, 2_000), Duration::from_millis(3_000));
        assert_eq!(idle_from_ticks(1_000, u32::MAX - 999), Duration::from_millis(2_000));
        assert_eq!(idle_from_ticks(7, 7), Duration::ZERO);
    }

    #[test]
    fn autostart_command_is_quoted() {
        let cmd = autostart_command(Path::new(r"C:\Program Files\Notify\notify.exe"));
        assert_eq!(cmd, r#""C:\Program Files\Notify\notify.exe""#);
    }

    #[test]
    fn registry_run_value_roundtrip() {
        // eigener Wertname, damit ein echter Autostart-Eintrag nie berührt wird
        let name = format!("NotifyTest-{}", std::process::id());
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                delete_run_value(&self.0).ok();
            }
        }
        let _cleanup = Cleanup(name.clone());

        assert_eq!(run_value(&name), None);
        set_run_value(&name, r#""C:\Test Ordner\x.exe" --flag"#).unwrap();
        assert_eq!(run_value(&name).as_deref(), Some(r#""C:\Test Ordner\x.exe" --flag"#));
        set_run_value(&name, "neu").unwrap();
        assert_eq!(run_value(&name).as_deref(), Some("neu"));
        delete_run_value(&name).unwrap();
        assert_eq!(run_value(&name), None);
        // nochmal entfernen ist kein Fehler
        delete_run_value(&name).unwrap();
    }

    #[test]
    fn lock_and_power_messages_set_the_flags() {
        // Die Flags sind global, darum alles in einem Test
        handle_message(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK);
        assert!(session_away());
        handle_message(WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK);
        assert!(!session_away());

        handle_message(WM_POWERBROADCAST, PBT_APMSUSPEND);
        assert!(session_away());
        // Nach dem Aufwachen mit gesperrtem Bildschirm bleibt es "weg"
        handle_message(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK);
        handle_message(WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC);
        assert!(session_away());
        handle_message(WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK);
        assert!(!session_away());

        // Fremde Nachrichten ändern nichts
        handle_message(0x0001, WTS_SESSION_LOCK);
        handle_message(WM_POWERBROADCAST, 0x99);
        assert!(!session_away());
    }
}
