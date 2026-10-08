use std::fs;
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

/// Position und Zustand der Leiste
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BarState {
    pub pos: Option<(i32, i32)>,
    pub collapsed: bool,
}

/// Format der Datei `bar`: `x,y,c` mit `c` = 1 (eingeklappt) oder 0
fn parse_bar(text: &str) -> Option<BarState> {
    let mut parts = text.trim().split(',');
    let x = parts.next()?.trim().parse().ok()?;
    let y = parts.next()?.trim().parse().ok()?;
    let collapsed = match parts.next().map(str::trim) {
        Some("1") => true,
        Some("0") | None => false,
        Some(_) => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(BarState { pos: Some((x, y)), collapsed })
}

fn format_bar(x: i32, y: i32, collapsed: bool) -> String {
    format!("{x},{y},{}", u8::from(collapsed))
}

pub fn load_bar() -> BarState {
    fs::read_to_string(dir().join("bar")).ok().and_then(|t| parse_bar(&t)).unwrap_or_default()
}

pub fn save_bar(x: i32, y: i32, collapsed: bool) {
    fs::write(dir().join("bar"), format_bar(x, y, collapsed)).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_state_roundtrip() {
        let text = format_bar(-120, 40, true);
        assert_eq!(text, "-120,40,1");
        assert_eq!(parse_bar(&text), Some(BarState { pos: Some((-120, 40)), collapsed: true }));
        assert_eq!(parse_bar("5,6,0\n"), Some(BarState { pos: Some((5, 6)), collapsed: false }));
        // alte Datei ohne Zustand
        assert_eq!(parse_bar("5,6"), Some(BarState { pos: Some((5, 6)), collapsed: false }));
    }

    #[test]
    fn bad_bar_state_is_rejected() {
        for bad in ["", "x,y", "1", "1,2,3", "1,2,0,9", "1.5,2,0"] {
            assert_eq!(parse_bar(bad), None, "{bad}");
        }
        assert_eq!(BarState::default(), BarState { pos: None, collapsed: false });
    }
}
