use crate::settings::SettingsInput;
use crate::tray::UserEvent;
use crate::widget::js_string;
use serde::Deserialize;
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{WebView, WebViewBuilder};

const NOTE_SIZE: LogicalSize<f64> = LogicalSize::new(440.0, 360.0);
const REFLECTION_SIZE: LogicalSize<f64> = LogicalSize::new(440.0, 560.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Note,
    Reflection,
}

impl Mode {
    fn js(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Reflection => "reflection",
        }
    }

    fn size(self) -> LogicalSize<f64> {
        match self {
            Self::Note => NOTE_SIZE,
            Self::Reflection => REFLECTION_SIZE,
        }
    }
}

pub enum EntryMsg {
    Save(String),
    Reflect { review: String, reflection: String, mood: String },
    Mode(Mode),
    Later,
    Skip,
    Close,
}

#[derive(Deserialize)]
struct ReflectPayload {
    review: String,
    reflection: String,
    mood: String,
}

fn parse_entry_msg(msg: &str) -> Option<EntryMsg> {
    if let Some(text) = msg.strip_prefix("save:") {
        return Some(EntryMsg::Save(text.to_string()));
    }
    if let Some(json) = msg.strip_prefix("reflect:") {
        let p: ReflectPayload = serde_json::from_str(json).ok()?;
        return Some(EntryMsg::Reflect { review: p.review, reflection: p.reflection, mood: p.mood });
    }
    match msg {
        "mode:note" => Some(EntryMsg::Mode(Mode::Note)),
        "later" => Some(EntryMsg::Later),
        "skip" => Some(EntryMsg::Skip),
        "close" => Some(EntryMsg::Close),
        _ => None,
    }
}

/// Nachrichten aus dem Fenster "Heutige Einträge"
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TodayMsg {
    Nav { delta: i64 },
    Today,
    UpdateNote { id: String, text: String },
    UpdateReflection { id: String, review: String, reflection: String, mood: String },
    Delete { id: String },
    Close,
}

/// Nachrichten aus dem Einstellungsfenster
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SettingsMsg {
    Save(Box<SettingsInput>),
    Close,
}

pub fn parse_today_msg(body: &str) -> Option<TodayMsg> {
    serde_json::from_str(body).ok()
}

pub fn parse_settings_msg(body: &str) -> Option<SettingsMsg> {
    serde_json::from_str(body).ok()
}

/// Normales Fenster mit WebView, versteckt bis zum ersten Öffnen
pub struct Panel {
    window: Window,
    webview: WebView,
    visible: bool,
}

impl Panel {
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        title: &str,
        html: &'static str,
        size: LogicalSize<f64>,
        on_message: impl Fn(String) + 'static,
    ) -> Result<Self, String> {
        let window = WindowBuilder::new()
            .with_title(title)
            .with_inner_size(size)
            .with_min_inner_size(LogicalSize::new(420.0, 360.0))
            .with_visible(false)
            .build(target)
            .map_err(|e| format!("Fenster {title}: {e}"))?;
        let webview = WebViewBuilder::new()
            .with_html(html)
            .with_ipc_handler(move |req| on_message(req.body().to_string()))
            .build(&window)
            .map_err(|e| format!("Fenster {title} (WebView): {e}"))?;
        Ok(Self { window, webview, visible: false })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn show(&mut self) {
        if !self.visible {
            if let Some(monitor) = self.window.primary_monitor() {
                let (pos, mon, win) = (monitor.position(), monitor.size(), self.window.outer_size());
                self.window.set_outer_position(PhysicalPosition::new(
                    pos.x as f64 + (mon.width as f64 - win.width as f64) / 2.0,
                    pos.y as f64 + (mon.height as f64 - win.height as f64) / 2.0,
                ));
            }
            self.window.set_visible(true);
            self.visible = true;
        }
        self.window.set_focus();
        self.webview.focus().ok();
    }

    pub fn hide(&mut self) {
        self.window.set_visible(false);
        self.visible = false;
    }

    pub fn script(&self, js: &str) {
        self.webview.evaluate_script(js).ok();
    }
}

/// Kleines Eingabefenster. Es bleibt versteckt, bis es angefordert wird.
pub struct EntryWindow {
    window: Window,
    webview: WebView,
    visible: bool,
    mode: Mode,
}

impl EntryWindow {
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Result<Self, String> {
        let window = WindowBuilder::new()
            .with_title("Eintrag")
            .with_inner_size(NOTE_SIZE)
            .with_resizable(false)
            .with_maximizable(false)
            .with_minimizable(false)
            .with_always_on_top(true)
            .with_visible(false)
            .build(target)
            .map_err(|e| format!("Eingabefenster: {e}"))?;

        let webview = WebViewBuilder::new()
            .with_html(include_str!("./ui/entry.html"))
            .with_ipc_handler(move |req| {
                if let Some(msg) = parse_entry_msg(req.body()) {
                    proxy.send_event(UserEvent::Entry(msg)).ok();
                }
            })
            .build(&window)
            .map_err(|e| format!("Eingabefenster (WebView): {e}"))?;

        Ok(Self { window, webview, visible: false, mode: Mode::Note })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Wechselt zwischen Notiz und Wochenreflexion und passt die Fenstergrösse an
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.window.set_inner_size(mode.size());
        self.webview.evaluate_script(&format!("onMode(\"{}\")", mode.js())).ok();
    }

    pub fn show(&mut self) {
        if !self.visible {
            self.center();
            self.window.set_visible(true);
        }
        self.visible = true;
        self.window.set_focus();
        self.webview.focus().ok();
        self.webview.evaluate_script("onShow()").ok();
    }

    pub fn hide(&mut self) {
        self.window.set_visible(false);
        self.visible = false;
    }

    /// Zeigt die Commits seit dem letzten Eintrag über dem Textfeld
    pub fn set_commits(&self, commits: &[(String, String)]) {
        let list: Vec<_> = commits
            .iter()
            .map(|(repo, text)| serde_json::json!({ "repo": repo, "text": text }))
            .collect();
        self.webview.evaluate_script(&format!("onCommits({})", serde_json::Value::Array(list))).ok();
    }

    pub fn reset(&self) {
        self.webview.evaluate_script("onReset()").ok();
    }

    pub fn error(&self, msg: &str) {
        self.webview.evaluate_script(&format!("onError({})", js_string(msg))).ok();
    }

    fn center(&self) {
        let Some(monitor) = self.window.primary_monitor() else { return };
        let (pos, mon) = (monitor.position(), monitor.size());
        let win = self.window.outer_size();
        self.window.set_outer_position(PhysicalPosition::new(
            pos.x as f64 + (mon.width as f64 - win.width as f64) / 2.0,
            pos.y as f64 + (mon.height as f64 - win.height as f64) / 2.0,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_today_messages() {
        assert!(matches!(parse_today_msg(r#"{"op":"nav","delta":-1}"#), Some(TodayMsg::Nav { delta: -1 })));
        assert!(matches!(parse_today_msg(r#"{"op":"today"}"#), Some(TodayMsg::Today)));
        assert!(matches!(parse_today_msg(r#"{"op":"close"}"#), Some(TodayMsg::Close)));
        assert!(matches!(
            parse_today_msg(r#"{"op":"update_note","id":"a-1","text":"neu\nzwei"}"#),
            Some(TodayMsg::UpdateNote { id, text }) if id == "a-1" && text == "neu\nzwei"
        ));
        assert!(matches!(
            parse_today_msg(r#"{"op":"update_reflection","id":"x","review":"r","reflection":"f","mood":"m"}"#),
            Some(TodayMsg::UpdateReflection { mood, .. }) if mood == "m"
        ));
        assert!(matches!(parse_today_msg(r#"{"op":"delete","id":"x"}"#), Some(TodayMsg::Delete { id }) if id == "x"));
        assert!(parse_today_msg(r#"{"op":"unbekannt"}"#).is_none());
        assert!(parse_today_msg(r#"{"op":"delete"}"#).is_none());
        assert!(parse_today_msg("kein json").is_none());
    }

    #[test]
    fn parses_settings_messages() {
        let save = r#"{"op":"save","work_days":["Mon","Fri"],"work_blocks":[{"start":"08:00","end":"12:00"}],
            "interval_minutes":"60","away_minutes":"10","school_days":["Wed"],"reflection_day":"Fri",
            "git_folders":["C:\\Work"],"git_emails":["a@b.ch"],"location":"@Firma","hours_per_day":"8.4",
            "export_dir":"","autostart":true}"#;
        assert!(matches!(
            parse_settings_msg(save),
            Some(SettingsMsg::Save(i)) if i.work_days.len() == 2 && i.autostart && i.git_folders == ["C:\\Work"]
        ));
        assert!(matches!(parse_settings_msg(r#"{"op":"close"}"#), Some(SettingsMsg::Close)));
        assert!(parse_settings_msg(r#"{"op":"save","work_days":["Montag"]}"#).is_none());
        assert!(parse_settings_msg("{}").is_none());
    }

    #[test]
    fn parses_reflection_and_mode_messages() {
        let msg = r#"reflect:{"review":"viel","reflection":"a\nb","mood":"gut"}"#;
        assert!(matches!(
            parse_entry_msg(msg),
            Some(EntryMsg::Reflect { review, reflection, mood })
                if review == "viel" && reflection == "a\nb" && mood == "gut"
        ));
        assert!(parse_entry_msg("reflect:kein json").is_none());
        assert!(parse_entry_msg(r#"reflect:{"review":"x"}"#).is_none());
        assert!(matches!(parse_entry_msg("mode:note"), Some(EntryMsg::Mode(Mode::Note))));
        assert!(parse_entry_msg("mode:anderes").is_none());
    }

    #[test]
    fn parses_entry_messages() {
        assert!(matches!(parse_entry_msg("save:hallo\nwelt"), Some(EntryMsg::Save(t)) if t == "hallo\nwelt"));
        assert!(matches!(parse_entry_msg("save:"), Some(EntryMsg::Save(t)) if t.is_empty()));
        assert!(matches!(parse_entry_msg("later"), Some(EntryMsg::Later)));
        assert!(matches!(parse_entry_msg("skip"), Some(EntryMsg::Skip)));
        assert!(matches!(parse_entry_msg("close"), Some(EntryMsg::Close)));
        assert!(parse_entry_msg("etwas anderes").is_none());
    }
}
