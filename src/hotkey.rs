use crate::log;
use crate::tray::UserEvent;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tao::event_loop::EventLoopProxy;

pub const DEFAULT: &str = "Ctrl+Alt+J";

fn letter_code(c: char) -> Option<Code> {
    Some(match c {
        'A' => Code::KeyA, 'B' => Code::KeyB, 'C' => Code::KeyC, 'D' => Code::KeyD, 'E' => Code::KeyE,
        'F' => Code::KeyF, 'G' => Code::KeyG, 'H' => Code::KeyH, 'I' => Code::KeyI, 'J' => Code::KeyJ,
        'K' => Code::KeyK, 'L' => Code::KeyL, 'M' => Code::KeyM, 'N' => Code::KeyN, 'O' => Code::KeyO,
        'P' => Code::KeyP, 'Q' => Code::KeyQ, 'R' => Code::KeyR, 'S' => Code::KeyS, 'T' => Code::KeyT,
        'U' => Code::KeyU, 'V' => Code::KeyV, 'W' => Code::KeyW, 'X' => Code::KeyX, 'Y' => Code::KeyY,
        'Z' => Code::KeyZ,
        '0' => Code::Digit0, '1' => Code::Digit1, '2' => Code::Digit2, '3' => Code::Digit3,
        '4' => Code::Digit4, '5' => Code::Digit5, '6' => Code::Digit6, '7' => Code::Digit7,
        '8' => Code::Digit8, '9' => Code::Digit9,
        _ => return None,
    })
}

fn function_code(n: u32) -> Option<Code> {
    Some(match n {
        1 => Code::F1, 2 => Code::F2, 3 => Code::F3, 4 => Code::F4, 5 => Code::F5, 6 => Code::F6,
        7 => Code::F7, 8 => Code::F8, 9 => Code::F9, 10 => Code::F10, 11 => Code::F11, 12 => Code::F12,
        _ => return None,
    })
}

fn key_code(token: &str) -> Option<Code> {
    let upper = token.to_uppercase();
    let mut chars = upper.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => letter_code(c),
        (Some('F'), Some(_)) => function_code(upper[1..].parse().ok()?),
        _ => None,
    }
}

/// `Ctrl+Alt+J`, `Strg+Shift+F9`: mindestens ein Modifier (Ctrl/Strg, Alt, Shift, Win) und genau eine Taste (A-Z, 0-9, F1-F12)
pub fn parse_hotkey(text: &str) -> Result<HotKey, String> {
    let bad = || format!("\"{}\" ist kein gültiges Tastenkürzel (Beispiel: Ctrl+Alt+J)", text.trim());
    let mut mods = Modifiers::empty();
    let mut key = None;
    for token in text.split('+').map(str::trim) {
        let modifier = match token.to_lowercase().as_str() {
            "ctrl" | "strg" | "control" => Some(Modifiers::CONTROL),
            "alt" => Some(Modifiers::ALT),
            "shift" | "umschalt" => Some(Modifiers::SHIFT),
            "win" | "super" | "meta" => Some(Modifiers::SUPER),
            _ => None,
        };
        match modifier {
            Some(m) if !mods.contains(m) => mods |= m,
            Some(_) => return Err(bad()),
            None if key.is_none() => key = Some(key_code(token).ok_or_else(bad)?),
            None => return Err(bad()),
        }
    }
    match key {
        Some(key) if !mods.is_empty() => Ok(HotKey::new(Some(mods), key)),
        _ => Err(bad()),
    }
}

/// Der globale Hotkey. Er lässt sich zur Laufzeit ändern, ein belegtes Kürzel wird nur geloggt.
pub struct Hotkey {
    manager: Option<GlobalHotKeyManager>,
    current: Option<HotKey>,
    id: Arc<AtomicU32>,
}

impl Hotkey {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        let id = Arc::new(AtomicU32::new(0));
        let watched = id.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if e.id == watched.load(Ordering::Relaxed) && e.state == HotKeyState::Pressed {
                proxy.send_event(UserEvent::Hotkey).ok();
            }
        }));
        let manager = GlobalHotKeyManager::new()
            .map_err(|e| log::warn(&format!("Tastenkürzel nicht verfügbar: {e}")))
            .ok();
        Self { manager, current: None, id }
    }

    /// Ersetzt das Kürzel. Fehler und Konflikte landen im Log, die App läuft weiter.
    pub fn set(&mut self, text: &str) {
        let Some(manager) = &self.manager else { return };
        if let Some(old) = self.current.take() {
            manager.unregister(old).ok();
        }
        self.id.store(0, Ordering::Relaxed);
        let hotkey = match parse_hotkey(text) {
            Ok(hotkey) => hotkey,
            Err(e) => {
                log::warn(&format!("Tastenkürzel nicht registriert: {e}"));
                return;
            }
        };
        match manager.register(hotkey) {
            Ok(()) => {
                self.id.store(hotkey.id(), Ordering::Relaxed);
                self.current = Some(hotkey);
            }
            Err(e) => log::warn(&format!("Tastenkürzel {text} nicht registriert (belegt?): {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_hotkeys() {
        let key = parse_hotkey("Ctrl+Alt+J").unwrap();
        assert_eq!((key.mods, key.key), (Modifiers::CONTROL | Modifiers::ALT, Code::KeyJ));
        assert_eq!(parse_hotkey(" strg + alt + j ").unwrap(), key);
        assert_eq!(parse_hotkey("Alt+Ctrl+J").unwrap(), key);
        let f9 = parse_hotkey("Shift+Win+F9").unwrap();
        assert_eq!((f9.mods, f9.key), (Modifiers::SHIFT | Modifiers::SUPER, Code::F9));
        assert_eq!(parse_hotkey("Ctrl+5").unwrap().key, Code::Digit5);
        assert_eq!(parse_hotkey(DEFAULT).unwrap(), key);
    }

    #[test]
    fn rejects_bad_hotkeys() {
        for bad in ["", "J", "Ctrl", "Ctrl+", "Ctrl+Alt", "Ctrl+J+K", "Ctrl+Ctrl+J", "Ctrl+Enter", "Ctrl+F13", "Ctrl+F0", "Ctrl+Ä"] {
            assert!(parse_hotkey(bad).is_err(), "{bad}");
        }
        assert!(parse_hotkey("Ctrl+x").unwrap().key == Code::KeyX);
        assert!(parse_hotkey("Ctrl+Mond").unwrap_err().contains("Tastenkürzel"));
    }
}
