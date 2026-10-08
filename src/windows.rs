use crate::layout::Rect;
use crate::settings::SettingsInput;
use crate::system;
use crate::tray::UserEvent;
use chrono::NaiveDate;
use serde::Deserialize;
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event::WindowEvent;
use tao::event_loop::EventLoopWindowTarget;
use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{WebContext, WebView, WebViewBuilder};

/// Gemeinsame Tokens und Bausteine aller Fenster
const BASE_CSS: &str = include_str!("./ui/base.css");

/// Setzt das gemeinsame CSS an die Stelle von `<!--BASE-->`
pub fn page(html: &str) -> String {
    html.replace("<!--BASE-->", &format!("<style>{BASE_CSS}</style>"))
}

pub fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Nachrichten von Pille und Panel
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BarMsg {
    /// Die Seite ist geladen und will den Zustand sehen
    Ready,
    Nav { delta: i64 },
    Today,
    SetOrt { ort: String },
    Add {
        text: String,
        #[serde(default)]
        hours: String,
    },
    Update { id: String, text: String },
    Delete { id: String },
    Rest,
    /// Pille: Panel ein- oder ausklappen
    Collapse { collapsed: bool },
    /// Pille: Zahnrad (öffnet oder schliesst die Einstellungen)
    Settings,
    /// Pille: Claude-Limits-Ring (öffnet oder schliesst die Detailkarte)
    Limits,
    Week,
    Hide,
    Drag,
}

/// Nachrichten der Wochenansicht
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum WeekMsg {
    Nav { delta: i64 },
    ThisWeek,
    SetOrt { date: NaiveDate, ort: String },
    Whole { kind: WholeWeek },
    SetTexts { rueckblick: String, reflexion: String, stimmung: String },
    Export,
    Drag,
    Close,
}

/// "Ganze Woche = ..."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WholeWeek {
    Ferien,
    Uek,
    /// Alle Overrides der Woche entfernen: es gilt der Wochenplan
    Plan,
}

/// Was der Auswahldialog wählen soll
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PickKind {
    Folder,
    /// Eine Word-Vorlage (.docx)
    Docx,
}

/// Nachrichten aus dem Einstellungs-Popover
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SettingsMsg {
    Save(Box<SettingsInput>),
    SaveTemplate,
    /// Ordner oder Datei wählen; das Ergebnis geht an das Feld `field`
    Pick { field: String, kind: PickKind },
    /// Die Seite meldet, ob gerade ein ungespeicherter Wert ungültig ist (dann bleibt das Popover bei Fokusverlust offen)
    Invalid { invalid: bool },
    /// Höhe: der Tab "Vorlage" ist höher als die anderen
    Tall { tall: bool },
    Close,
}

/// Nachrichten der Detailkarte der Claude-Limits
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LimitsMsg {
    Close,
}

/// Nachrichten des Hinweisfensters
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ReminderMsg {
    Now,
    Snooze,
    Close,
}

pub fn parse_bar_msg(body: &str) -> Option<BarMsg> {
    serde_json::from_str(body).ok()
}

pub fn parse_week_msg(body: &str) -> Option<WeekMsg> {
    serde_json::from_str(body).ok()
}

pub fn parse_settings_msg(body: &str) -> Option<SettingsMsg> {
    serde_json::from_str(body).ok()
}

pub fn parse_limits_msg(body: &str) -> Option<LimitsMsg> {
    serde_json::from_str(body).ok()
}

pub fn parse_reminder_msg(body: &str) -> Option<ReminderMsg> {
    serde_json::from_str(body).ok()
}

/// Rahmenloses, rundes Fenster mit WebView, immer im Vordergrund und ohne Taskleisteneintrag.
/// Der WebView ist durchsichtig, zusätzlich schneidet eine runde Fensterregion die Ecken sauber aus:
/// so bleiben keine eckigen Kanten oder hellen Pixel sichtbar.
pub struct Popup {
    window: Window,
    webview: WebView,
    visible: bool,
    /// Eckenradius in logischen Pixeln
    radius: f64,
}

impl Popup {
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        title: &str,
        html: &str,
        size: (f64, f64),
        radius: f64,
        on_message: impl Fn(String) + 'static,
    ) -> Result<Self, String> {
        let window = WindowBuilder::new()
            .with_title(title)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top(true)
            .with_resizable(false)
            .with_skip_taskbar(true)
            .with_undecorated_shadow(false)
            .with_focused(false)
            .with_visible(false)
            .with_inner_size(LogicalSize::new(size.0, size.1))
            .build(target)
            .map_err(|e| format!("Fenster {title}: {e}"))?;
        // Standardordner liegt neben der exe und ist unter Program Files nicht beschreibbar
        let mut context = WebContext::new(Some(crate::store::dir().join("webview")));
        let webview = WebViewBuilder::new_with_web_context(&mut context)
            .with_transparent(true)
            .with_html(page(html))
            .with_ipc_handler(move |req| on_message(req.body().to_string()))
            .build(&window)
            .map_err(|e| format!("Fenster {title} (WebView): {e}"))?;
        let popup = Self { window, webview, visible: false, radius };
        popup.round();
        Ok(popup)
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn hwnd(&self) -> isize {
        self.window.hwnd()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Hat das Fenster (oder ein Dialog, der ihm gehört) den Fokus?
    pub fn is_foreground(&self) -> bool {
        self.visible && system::is_foreground_family(self.window.hwnd())
    }

    /// Zeigt das Fenster. Mit `activate` bekommt es den Fokus, sonst bleibt der Fokus, wo er war.
    pub fn show(&mut self, activate: bool) {
        self.visible = true;
        if activate {
            self.window.set_visible(true);
            self.window.set_always_on_top(true);
            self.window.set_focus();
            self.webview.focus().ok();
        } else {
            system::show_no_activate(self.window.hwnd());
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.window.set_visible(false);
            // tao kennt Fenster nicht, die `show_no_activate` eingeblendet hat: direkt ausblenden
            system::hide_window(self.window.hwnd());
            self.visible = false;
        }
    }

    pub fn script(&self, js: &str) {
        self.webview.evaluate_script(js).ok();
    }

    pub fn focus_webview(&self) {
        self.webview.focus().ok();
    }

    pub fn drag(&self) {
        self.window.drag_window().ok();
    }

    /// Setzt die Grösse in logischen Pixeln und schneidet die Ecken neu aus
    pub fn set_size(&self, size: (f64, f64)) {
        self.window.set_inner_size(LogicalSize::new(size.0, size.1));
        self.round();
    }

    pub fn move_to(&self, x: i32, y: i32) {
        self.window.set_outer_position(PhysicalPosition::new(x, y));
    }

    pub fn position(&self) -> Option<(i32, i32)> {
        self.window.outer_position().ok().map(|p| (p.x, p.y))
    }

    /// Grösse in physischen Pixeln
    pub fn size_px(&self) -> (i32, i32) {
        let s = self.window.outer_size();
        (s.width as i32, s.height as i32)
    }

    /// Liegt der Punkt (mit `margin` Pixeln Abstand zur Monitorecke unten rechts) auf einem angeschlossenen Monitor?
    pub fn on_any_monitor(&self, x: i32, y: i32, margin: i32) -> bool {
        self.window.available_monitors().any(|m| {
            let (p, s) = (m.position(), m.size());
            x >= p.x && y >= p.y && x < p.x + s.width as i32 - margin && y < p.y + s.height as i32 - margin
        })
    }

    pub fn scale(&self) -> f64 {
        self.window.scale_factor()
    }

    /// Arbeitsbereich (ohne Taskleiste) des Monitors, auf dem das Fenster liegt
    pub fn work_area(&self) -> Option<Rect> {
        system::work_area(self.window.hwnd())
    }

    pub fn rect(&self) -> Option<Rect> {
        let (x, y) = self.position()?;
        let (w, h) = self.size_px();
        Some(Rect::new(x, y, w, h))
    }

    /// Runde Fensterregion passend zur aktuellen Grösse
    fn round(&self) {
        let (w, h) = self.size_px();
        let radius = (self.radius * self.scale()).round() as i32;
        system::round_window(self.window.hwnd(), w, h, radius);
    }

    /// Schneidet die Ecken neu aus, wenn sich Grösse oder Skalierung ändern
    pub fn handle_event(&self, id: WindowId, event: &WindowEvent) {
        if id == self.window.id() && matches!(event, WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }) {
            self.round();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bar_messages() {
        assert!(matches!(parse_bar_msg(r#"{"op":"ready"}"#), Some(BarMsg::Ready)));
        assert!(matches!(parse_bar_msg(r#"{"op":"nav","delta":-1}"#), Some(BarMsg::Nav { delta: -1 })));
        assert!(matches!(parse_bar_msg(r#"{"op":"today"}"#), Some(BarMsg::Today)));
        assert!(matches!(parse_bar_msg(r#"{"op":"set_ort","ort":"Gibb"}"#), Some(BarMsg::SetOrt { ort }) if ort == "Gibb"));
        assert!(matches!(
            parse_bar_msg(r#"{"op":"add","text":"Bug gefixt 2.4"}"#),
            Some(BarMsg::Add { text, hours }) if text == "Bug gefixt 2.4" && hours.is_empty()
        ));
        assert!(matches!(
            parse_bar_msg(r#"{"op":"add","text":"a\nb","hours":"1,5"}"#),
            Some(BarMsg::Add { text, hours }) if text == "a\nb" && hours == "1,5"
        ));
        assert!(matches!(
            parse_bar_msg(r#"{"op":"update","id":"a-1","text":"neu 2"}"#),
            Some(BarMsg::Update { id, text }) if id == "a-1" && text == "neu 2"
        ));
        assert!(matches!(parse_bar_msg(r#"{"op":"delete","id":"x"}"#), Some(BarMsg::Delete { id }) if id == "x"));
        assert!(matches!(parse_bar_msg(r#"{"op":"rest"}"#), Some(BarMsg::Rest)));
        assert!(matches!(parse_bar_msg(r#"{"op":"collapse","collapsed":true}"#), Some(BarMsg::Collapse { collapsed: true })));
        assert!(matches!(parse_bar_msg(r#"{"op":"collapse","collapsed":false}"#), Some(BarMsg::Collapse { collapsed: false })));
        assert!(matches!(parse_bar_msg(r#"{"op":"settings"}"#), Some(BarMsg::Settings)));
        assert!(matches!(parse_bar_msg(r#"{"op":"limits"}"#), Some(BarMsg::Limits)));
        assert!(matches!(parse_bar_msg(r#"{"op":"week"}"#), Some(BarMsg::Week)));
        assert!(matches!(parse_bar_msg(r#"{"op":"hide"}"#), Some(BarMsg::Hide)));
        assert!(matches!(parse_bar_msg(r#"{"op":"drag"}"#), Some(BarMsg::Drag)));
        assert!(parse_bar_msg(r#"{"op":"unbekannt"}"#).is_none());
        assert!(parse_bar_msg(r#"{"op":"delete"}"#).is_none());
        assert!(parse_bar_msg(r#"{"op":"collapse"}"#).is_none());
        assert!(parse_bar_msg(r#"{"op":"collapse","collapsed":"ja"}"#).is_none());
        assert!(parse_bar_msg("drag").is_none());
        assert!(parse_bar_msg("").is_none());
    }

    #[test]
    fn parses_week_messages() {
        assert!(matches!(parse_week_msg(r#"{"op":"nav","delta":1}"#), Some(WeekMsg::Nav { delta: 1 })));
        assert!(matches!(parse_week_msg(r#"{"op":"this_week"}"#), Some(WeekMsg::ThisWeek)));
        assert!(matches!(
            parse_week_msg(r#"{"op":"set_ort","date":"2026-10-08","ort":"üK"}"#),
            Some(WeekMsg::SetOrt { date, ort }) if date == NaiveDate::from_ymd_opt(2026, 10, 8).unwrap() && ort == "üK"
        ));
        assert!(matches!(parse_week_msg(r#"{"op":"whole","kind":"ferien"}"#), Some(WeekMsg::Whole { kind: WholeWeek::Ferien })));
        assert!(matches!(parse_week_msg(r#"{"op":"whole","kind":"uek"}"#), Some(WeekMsg::Whole { kind: WholeWeek::Uek })));
        assert!(matches!(parse_week_msg(r#"{"op":"whole","kind":"plan"}"#), Some(WeekMsg::Whole { kind: WholeWeek::Plan })));
        assert!(matches!(
            parse_week_msg(r#"{"op":"set_texts","rueckblick":"r","reflexion":"a\nb","stimmung":"gut"}"#),
            Some(WeekMsg::SetTexts { rueckblick, reflexion, stimmung }) if rueckblick == "r" && reflexion == "a\nb" && stimmung == "gut"
        ));
        assert!(matches!(parse_week_msg(r#"{"op":"export"}"#), Some(WeekMsg::Export)));
        assert!(matches!(parse_week_msg(r#"{"op":"drag"}"#), Some(WeekMsg::Drag)));
        assert!(matches!(parse_week_msg(r#"{"op":"close"}"#), Some(WeekMsg::Close)));
        assert!(parse_week_msg(r#"{"op":"whole","kind":"krank"}"#).is_none());
        assert!(parse_week_msg(r#"{"op":"set_ort","date":"morgen","ort":"x"}"#).is_none());
        assert!(parse_week_msg(r#"{"op":"set_texts","rueckblick":"r"}"#).is_none());
        assert!(parse_week_msg("kein json").is_none());
    }

    #[test]
    fn parses_settings_messages() {
        // Felder der alten Version (work_days, location, ...) werden ignoriert, die neuen sind optional
        let save = r#"{"op":"save","work_days":["Mon","Fri"],"work_blocks":[{"start":"08:00","end":"12:00"}],
            "interval_minutes":"60","away_minutes":"10","school_days":["Wed"],"reflection_day":"Fri",
            "git_folders":["C:\\Work"],"git_emails":["a@b.ch"],"location":"@Firma","hours_per_day":"8.4",
            "export_dir":"","autostart":true}"#;
        assert!(matches!(
            parse_settings_msg(save),
            Some(SettingsMsg::Save(i)) if i.work_blocks.len() == 1 && i.autostart && i.git_folders == ["C:\\Work"]
                && i.nachname.is_none() && i.wochenplan.is_none() && i.hotkey.is_none()
                && i.aufklappen.is_none() && i.snooze_minutes.is_none() && i.claude_limits.is_none()
                && i.ring_five_hour.is_none() && i.ring_seven_day.is_none() && i.ring_context.is_none()
                && i.ring_warn.is_none() && i.ring_crit.is_none() && i.ring_warn_at.is_none() && i.ring_crit_at.is_none()
        ));
        let v2 = r##"{"op":"save","work_blocks":[],"interval_minutes":"60","away_minutes":"10","reflection_day":"Fri",
            "git_folders":[],"git_emails":[],"export_dir":"","autostart":false,"nachname":"Maurer","vorname":"Jemuel",
            "rest_auffuellen":true,"wochenplan":[{"tag":"Mon","ort":"Gibb"}],
            "orte":[{"name":"Gibb","tagessoll":8.4,"art":"schule"}],"uek_texte":{"taetigkeit":"üK"},
            "journal_dir":"D:\\J","vorlage_pfad":"","hotkey":"Ctrl+Shift+K","commit_erinnerung":false,
            "aufklappen":"oben","snooze_minutes":"20","claude_limits":false,
            "ring_five_hour":"#ABC","ring_seven_day":"8aa8cc","ring_context":"#5c5c5c","ring_warn":"#d6b878","ring_crit":"",
            "ring_warn_at":"70","ring_crit_at":"90"}"##;
        assert!(matches!(
            parse_settings_msg(v2),
            Some(SettingsMsg::Save(i)) if i.nachname.as_deref() == Some("Maurer") && i.rest_auffuellen == Some(true)
                && i.wochenplan.as_ref().is_some_and(|w| w.len() == 1)
                && i.orte.as_ref().is_some_and(|o| o[0].tagessoll == 8.4)
                && i.uek_texte.as_ref().is_some_and(|t| t.taetigkeit == "üK" && !t.rueckblick.is_empty())
                && i.hotkey.as_deref() == Some("Ctrl+Shift+K") && i.commit_erinnerung == Some(false)
                && i.journal_dir.as_deref() == Some("D:\\J")
                && i.aufklappen.as_deref() == Some("oben") && i.snooze_minutes.as_deref() == Some("20")
                && i.claude_limits == Some(false)
                && i.ring_five_hour.as_deref() == Some("#ABC") && i.ring_seven_day.as_deref() == Some("8aa8cc")
                && i.ring_context.as_deref() == Some("#5c5c5c") && i.ring_warn.as_deref() == Some("#d6b878")
                && i.ring_crit.as_deref() == Some("")
                && i.ring_warn_at.as_deref() == Some("70") && i.ring_crit_at.as_deref() == Some("90")
        ));
        // Farbfelder mit falschem Typ lassen die ganze Nachricht scheitern
        assert!(parse_settings_msg(
            r#"{"op":"save","work_blocks":[],"interval_minutes":"1","away_minutes":"1","reflection_day":"Fri",
            "git_folders":[],"git_emails":[],"export_dir":"","autostart":false,"ring_warn":5}"#
        )
        .is_none());
        assert!(matches!(parse_settings_msg(r#"{"op":"close"}"#), Some(SettingsMsg::Close)));
        assert!(matches!(parse_settings_msg(r#"{"op":"save_template"}"#), Some(SettingsMsg::SaveTemplate)));
        assert!(parse_settings_msg(r#"{"op":"save","work_days":["Montag"]}"#).is_none());
        assert!(parse_settings_msg("{}").is_none());
    }

    #[test]
    fn parses_settings_popover_messages() {
        assert!(matches!(
            parse_settings_msg(r#"{"op":"pick","field":"journal-dir","kind":"folder"}"#),
            Some(SettingsMsg::Pick { field, kind: PickKind::Folder }) if field == "journal-dir"
        ));
        assert!(matches!(
            parse_settings_msg(r#"{"op":"pick","field":"vorlage","kind":"docx"}"#),
            Some(SettingsMsg::Pick { kind: PickKind::Docx, .. })
        ));
        assert!(parse_settings_msg(r#"{"op":"pick","field":"x","kind":"exe"}"#).is_none());
        assert!(parse_settings_msg(r#"{"op":"pick","kind":"folder"}"#).is_none());
        assert!(matches!(parse_settings_msg(r#"{"op":"invalid","invalid":true}"#), Some(SettingsMsg::Invalid { invalid: true })));
        assert!(matches!(parse_settings_msg(r#"{"op":"invalid","invalid":false}"#), Some(SettingsMsg::Invalid { invalid: false })));
        assert!(parse_settings_msg(r#"{"op":"invalid"}"#).is_none());
        assert!(matches!(parse_settings_msg(r#"{"op":"tall","tall":true}"#), Some(SettingsMsg::Tall { tall: true })));
        assert!(parse_settings_msg(r#"{"op":"tall","tall":1}"#).is_none());
    }

    #[test]
    fn parses_limits_messages() {
        assert!(matches!(parse_limits_msg(r#"{"op":"close"}"#), Some(LimitsMsg::Close)));
        assert!(parse_limits_msg(r#"{"op":"limits"}"#).is_none());
        assert!(parse_limits_msg("close").is_none());
    }

    #[test]
    fn parses_reminder_messages() {
        assert!(matches!(parse_reminder_msg(r#"{"op":"now"}"#), Some(ReminderMsg::Now)));
        assert!(matches!(parse_reminder_msg(r#"{"op":"snooze"}"#), Some(ReminderMsg::Snooze)));
        assert!(matches!(parse_reminder_msg(r#"{"op":"close"}"#), Some(ReminderMsg::Close)));
        assert!(parse_reminder_msg(r#"{"op":"later"}"#).is_none());
        assert!(parse_reminder_msg("now").is_none());
    }

    #[test]
    fn js_string_escapes_everything_dangerous() {
        assert_eq!(js_string("a\"b\\c\nd"), r#""a\"b\\c\nd""#);
        assert_eq!(js_string("</script>\u{2028}"), "\"</script>\\u2028\"");
    }

    #[test]
    fn page_inserts_the_base_css() {
        let html = page("<head><!--BASE--></head>");
        assert!(html.contains("--bg: #0a0a0a") && !html.contains("<!--BASE-->"));
        // der SVG-Namespace in einer Data-URI ist keine Netzwerkanfrage
        let css = BASE_CSS.replace("http://www.w3.org/2000/svg", "");
        assert!(!css.contains("http://") && !css.contains("https://") && !css.contains("@import"), "keine externen Ressourcen");
    }

    #[test]
    fn theme_tokens_match_the_design() {
        for token in [
            "--card: #111111",
            "--muted: #1c1c1c",
            "--mfg: #8a8a8a",
            "--fg: #ededed",
            "--line: rgba(255,255,255,.07)",
            "--input: rgba(255,255,255,.12)",
            "--noser: #8aa8cc",
            "--noser-d: #4d6d95",
            "--gibb: #a99bc7",
            "--gibb-d: #6b5b9a",
            "--uek: #c2a27e",
            "--uek-d: #8a6a43",
            "--ferien: #84b3a6",
            "--ferien-d: #3f7a6c",
            "--ok: #8fb59b",
            "--over: #d28f8f",
            "--hinweis: #d6b878",
        ] {
            assert!(BASE_CSS.contains(token), "{token}");
        }
        assert!(BASE_CSS.contains("\"Geist\", \"Segoe UI\", system-ui, sans-serif"));
    }

    #[test]
    fn pages_have_no_external_resources() {
        for (name, html) in [
            ("pill", include_str!("./ui/pill.html")),
            ("panel", include_str!("./ui/panel.html")),
            ("week", include_str!("./ui/week.html")),
            ("settings", include_str!("./ui/settings.html")),
            ("reminder", include_str!("./ui/reminder.html")),
            ("limits", include_str!("./ui/limits.html")),
        ] {
            for bad in ["src=\"http", "href=\"http", "url(http", "@import", "fonts.googleapis"] {
                assert!(!html.contains(bad), "{name}: {bad}");
            }
            assert!(html.contains("<!--BASE-->"), "{name}");
        }
    }
}
