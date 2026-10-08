use crate::journal::{Entry, Quelle};
use crate::layout::{self, GAP, MARGIN, Rect};
use crate::timer::{Schedule, checkins_allowed, in_work_time};
use crate::tray::UserEvent;
use crate::windows::{Popup, parse_reminder_msg};
use chrono::{Local, NaiveDateTime};
use std::time::{Duration, Instant};
use tao::event::WindowEvent;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::WindowId;

const SIZE: (f64, f64) = (400.0, 140.0);
const MAX_LAST: usize = 40;

/// Inhalt eines Hinweises
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub title: String,
    pub badge: Option<String>,
    pub text: String,
    pub button: String,
}

/// Hinweis zum Stundenintervall oder nach einem neuen Commit
pub fn checkin_hint(title: &str, commits: usize, entries: &[Entry], now: NaiveDateTime) -> Hint {
    Hint {
        title: title.to_string(),
        badge: commit_badge(commits),
        text: last_note_text(entries, now),
        button: "Jetzt eintragen".to_string(),
    }
}

/// Hinweis auf die fällige Wochenreflexion
pub fn reflection_hint() -> Hint {
    Hint {
        title: "Wochenrückblick fällig".to_string(),
        badge: None,
        text: "Rückblick, Reflexion und Stimmung der Woche schreiben.".to_string(),
        button: "Jetzt schreiben".to_string(),
    }
}

fn commit_badge(n: usize) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 neuer Commit".to_string()),
        n => Some(format!("{n} neue Commits")),
    }
}

/// "Seit 14:00 nichts notiert. Zuletzt: ..." aus dem letzten eigenen Eintrag des Tages
fn last_note_text(entries: &[Entry], now: NaiveDateTime) -> String {
    let today = now.date();
    let last = entries
        .iter()
        .filter(|e| e.quelle == Quelle::Manuell && e.local_date() == today)
        .max_by_key(|e| e.t);
    let Some(last) = last else { return "Heute noch nichts notiert.".to_string() };
    let time = last.t.with_timezone(&Local).format("%H:%M");
    let first_line = last.text.lines().next().unwrap_or("").trim();
    let shown: String = if first_line.chars().count() > MAX_LAST {
        first_line.chars().take(MAX_LAST).chain(std::iter::once('…')).collect()
    } else {
        first_line.to_string()
    };
    format!("Seit {time} nichts notiert. Zuletzt: {shown}")
}

/// Höchstens ein Hinweis pro Intervall, egal ob Stunden-Check-in oder Commit
#[derive(Debug, Default)]
pub struct Gate {
    last: Option<Instant>,
}

impl Gate {
    pub fn allows(&self, now: Instant, interval: Duration) -> bool {
        self.last.is_none_or(|t| now.duration_since(t) >= interval)
    }

    pub fn shown(&mut self, now: Instant) {
        self.last = Some(now);
    }
}

/// Darf ein neuer Commit jetzt einen Hinweis auslösen? Nur an Arbeitstagen, in der Arbeitszeit, wenn man nicht weg ist.
pub fn commit_hint_allowed(schedule: &Schedule, now: NaiveDateTime, override_name: Option<&str>, away: bool) -> bool {
    !away && checkins_allowed(now.date(), override_name, schedule) && in_work_time(schedule, now.time())
}

/// Kleines rundes, rahmenloses Fenster unten rechts. Es nimmt keinen Fokus, bis man hineinklickt.
pub struct ReminderWindow {
    popup: Popup,
}

impl ReminderWindow {
    pub fn new(target: &EventLoopWindowTarget<UserEvent>, proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        let popup = Popup::new(
            target,
            "Notify Hinweis",
            include_str!("./ui/reminder.html"),
            SIZE,
            20.0,
            move |body| {
                if let Some(msg) = parse_reminder_msg(&body) {
                    proxy.send_event(UserEvent::Reminder(msg)).ok();
                }
            },
        )?;
        Ok(Self { popup })
    }

    pub fn id(&self) -> WindowId {
        self.popup.id()
    }

    pub fn handle_event(&self, id: WindowId, event: &WindowEvent) {
        self.popup.handle_event(id, event);
    }

    /// `snooze_minutes`: Beschriftung des Knopfes "In ... min"
    pub fn show(&mut self, hint: &Hint, snooze_minutes: u32, avoid: Option<Rect>) {
        let data = serde_json::json!({
            "title": hint.title,
            "badge": hint.badge,
            "text": hint.text,
            "button": hint.button,
            "snooze": snooze_label(snooze_minutes),
        });
        self.popup.script(&format!("onHint({data})"));
        self.place(avoid);
        self.popup.show(false);
    }

    pub fn hide(&mut self) {
        self.popup.hide();
    }

    /// Unten rechts im Arbeitsbereich, ausser dort steht die Leiste (`avoid`)
    fn place(&self, avoid: Option<Rect>) {
        let Some(work) = self.popup.work_area() else { return };
        let scale = self.popup.scale();
        let margin = (MARGIN * scale).round() as i32;
        let gap = (GAP * scale).round() as i32;
        let (x, y) = layout::reminder_position(work, self.popup.size_px(), margin, avoid, gap);
        self.popup.move_to(x, y);
    }
}

/// "In 15 min", ab 60 Minuten "In 1 h" bzw. "In 1 h 30 min"
pub fn snooze_label(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("In {m} min"),
        (h, 0) => format!("In {h} h"),
        (h, m) => format!("In {h} h {m} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use chrono::{NaiveDate, TimeZone, Utc};

    fn schedule() -> Schedule {
        Settings::default().schedule().unwrap()
    }

    /// Oktober 2026: 5 = Montag, 8 = Donnerstag (Gibb), 10 = Samstag
    fn dt(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn at(d: u32, h: u32, m: u32) -> chrono::DateTime<Utc> {
        Local.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn gate_allows_one_hint_per_interval() {
        let t0 = Instant::now();
        let interval = Duration::from_secs(3600);
        let mut gate = Gate::default();
        assert!(gate.allows(t0, interval));
        gate.shown(t0);
        assert!(!gate.allows(t0 + Duration::from_secs(10), interval));
        assert!(!gate.allows(t0 + interval - Duration::from_secs(1), interval));
        assert!(gate.allows(t0 + interval, interval));
        gate.shown(t0 + interval);
        assert!(!gate.allows(t0 + interval + Duration::from_secs(60), interval));
    }

    #[test]
    fn commit_hint_needs_work_time_and_a_work_day() {
        let s = schedule();
        assert!(commit_hint_allowed(&s, dt(5, 10, 30), None, false));
        // Mittagspause, vor Arbeitsbeginn, nach Feierabend
        assert!(!commit_hint_allowed(&s, dt(5, 12, 30), None, false));
        assert!(!commit_hint_allowed(&s, dt(5, 7, 0), None, false));
        assert!(!commit_hint_allowed(&s, dt(5, 18, 0), None, false));
        // Gibb-Tag, Wochenende, Override auf Ferien
        assert!(!commit_hint_allowed(&s, dt(8, 10, 30), None, false));
        assert!(!commit_hint_allowed(&s, dt(10, 10, 30), None, false));
        assert!(!commit_hint_allowed(&s, dt(5, 10, 30), Some("Ferien"), false));
        // Override auf Arbeit am Gibb-Tag
        assert!(commit_hint_allowed(&s, dt(8, 10, 30), Some("Noser Young"), false));
        // weg (gesperrt oder lange keine Eingabe)
        assert!(!commit_hint_allowed(&s, dt(5, 10, 30), None, true));
    }

    #[test]
    fn snooze_label_reads_naturally() {
        assert_eq!(snooze_label(15), "In 15 min");
        assert_eq!(snooze_label(1), "In 1 min");
        assert_eq!(snooze_label(60), "In 1 h");
        assert_eq!(snooze_label(90), "In 1 h 30 min");
        assert_eq!(snooze_label(240), "In 4 h");
    }

    #[test]
    fn badge_counts_commits() {
        assert_eq!(commit_badge(0), None);
        assert_eq!(commit_badge(1).as_deref(), Some("1 neuer Commit"));
        assert_eq!(commit_badge(3).as_deref(), Some("3 neue Commits"));
    }

    #[test]
    fn text_names_the_last_note_of_today() {
        let entries = vec![
            Entry::manuell(at(5, 9, 0), "Alt", 1.0),
            Entry::manuell(at(5, 14, 0), "Kalender gebaut\nzweite Zeile", 2.0),
            Entry::commit(at(5, 15, 0), "recur", &"a".repeat(40), "Fix"),
            Entry::manuell(at(4, 16, 0), "Gestern", 1.0),
        ];
        assert_eq!(last_note_text(&entries, dt(5, 15, 30)), "Seit 14:00 nichts notiert. Zuletzt: Kalender gebaut");
        assert_eq!(last_note_text(&entries[3..], dt(5, 15, 30)), "Heute noch nichts notiert.");
        assert_eq!(last_note_text(&[], dt(5, 15, 30)), "Heute noch nichts notiert.");
        let long = vec![Entry::manuell(at(5, 9, 0), &"x".repeat(100), 0.0)];
        assert!(last_note_text(&long, dt(5, 10, 0)).ends_with(&format!("{}…", "x".repeat(MAX_LAST))));
    }

    #[test]
    fn hints_carry_title_badge_and_button() {
        let h = checkin_hint("Eintrag fällig", 2, &[], dt(5, 10, 0));
        assert_eq!((h.title.as_str(), h.badge.as_deref(), h.button.as_str()), ("Eintrag fällig", Some("2 neue Commits"), "Jetzt eintragen"));
        let r = reflection_hint();
        assert!(r.badge.is_none() && r.title.contains("Wochenrückblick"));
    }
}
