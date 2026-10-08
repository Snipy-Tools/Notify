use crate::bar::Bar;
use crate::export::{self, ExportConfig};
use crate::git::{self, Commit, GitConfig};
use crate::hotkey::Hotkey;
use crate::journal::{Entry, Journal, Quelle, Update, WeekTexts, iso_week, split_duration, week_bounds};
use crate::layout;
use crate::limits::{self, LimitsWatcher};
use crate::log;
use crate::reminder::{self, Gate, ReminderWindow};
use crate::settings::{DayKind, Settings, SettingsInput, check_git_folders};
use crate::system;
use crate::timer::{Schedule, Timer, checkins_allowed, reflection_wanted};
use crate::tray::UserEvent;
use crate::view;
use crate::windows::{
    BarMsg, LimitsMsg, PickKind, Popup, ReminderMsg, SettingsMsg, WeekMsg, WholeWeek, js_string, parse_limits_msg,
    parse_settings_msg, parse_week_msg,
};
use chrono::{Days, Local, NaiveDate, Utc};
use std::time::{Duration, Instant};
use tao::event::WindowEvent;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::WindowId;

const POLL: Duration = Duration::from_secs(15);
const GIT_EVERY: Duration = Duration::from_secs(5 * 60);
const GIT_STUCK: Duration = Duration::from_secs(5 * 60);
const SAVED_FOR: Duration = Duration::from_secs(10);
/// So lange nach einem Fokusverlust wartet das Popover, bevor es sich schliesst (Klicks auf das Zahnrad kommen dazwischen)
const BLUR_DELAY: Duration = Duration::from_millis(150);
/// Ein Klick aufs Zahnrad direkt nach dem Schliessen durch Fokusverlust öffnet nicht neu
const REOPEN_GUARD: Duration = Duration::from_millis(600);
/// Nach einem Dateidialog ignoriert das Popover Fokuswechsel noch kurz
/// So oft schaut die App auf den Fokus des offenen Popovers
const POPOVER_POLL: Duration = Duration::from_millis(250);
const DIALOG_GRACE: Duration = Duration::from_millis(700);
/// So oft schaut die App nach `claude-limits.json` (und frischt die offene Karte auf)
const LIMITS_POLL: Duration = Duration::from_secs(5);

const SETTINGS_SIZE: (f64, f64) = (760.0, 620.0);
const SETTINGS_SIZE_TALL: (f64, f64) = (760.0, 700.0);
const WEEK_SIZE: (f64, f64) = (760.0, 700.0);
const LIMITS_SIZE: (f64, f64) = (340.0, 320.0);

/// Zustand des Einstellungs-Popovers: wann es sich bei Fokusverlust schliessen darf
#[derive(Debug, Default)]
pub struct PopoverGuard {
    /// Der Tab "Vorlage" ist höher
    tall: bool,
    /// Die Seite meldet einen ungültigen, ungespeicherten Wert
    invalid: bool,
    /// Ein Datei- oder Ordnerdialog ist offen
    dialog_open: bool,
    ignore_blur_until: Option<Instant>,
    blur_check_at: Option<Instant>,
    closed_by_blur_at: Option<Instant>,
    /// Das Popover hatte seit dem Öffnen mindestens einmal den Fokus
    seen_foreground: bool,
}

/// Was beim Fokusverlust des Popovers passiert
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlurAction {
    /// Offen lassen
    Keep,
    /// Offen lassen und den Fehler erneut zeigen
    ShowError,
    Close,
}

impl PopoverGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Beim Öffnen: alles zurück auf Anfang (ausser der Sperre gegen sofortiges Wiederöffnen)
    pub fn reset(&mut self) {
        let closed = self.closed_by_blur_at;
        *self = Self::default();
        self.closed_by_blur_at = closed;
    }

    pub fn size(&self) -> (f64, f64) {
        if self.tall { SETTINGS_SIZE_TALL } else { SETTINGS_SIZE }
    }

    /// Fokus ist weg: in `BLUR_DELAY` prüfen, ob er wirklich woanders ist
    pub fn focus_lost(&mut self, now: Instant) {
        self.blur_check_at = Some(now + BLUR_DELAY);
    }

    pub fn focus_gained(&mut self) {
        self.blur_check_at = None;
    }

    /// Regelmässiger Blick auf den Vordergrund, solange das Popover offen ist: Fokusverlust erkennen, auch wenn Windows
    /// keinen Event schickt (z. B. wenn das Popover nie den Fokus bekommen hat, zählt erst der Moment, in dem es ihn hatte)
    pub fn observe(&mut self, now: Instant, foreground: bool) {
        if foreground {
            self.seen_foreground = true;
            self.blur_check_at = None;
        } else if self.seen_foreground && self.blur_check_at.is_none() {
            self.blur_check_at = Some(now + BLUR_DELAY);
        }
    }

    pub fn dialog_open(&self) -> bool {
        self.dialog_open
    }

    pub fn set_invalid(&mut self, invalid: bool) {
        self.invalid = invalid;
    }

    /// Gibt `true` zurück, wenn sich die Höhe ändert
    pub fn set_tall(&mut self, tall: bool) -> bool {
        std::mem::replace(&mut self.tall, tall) != tall
    }

    pub fn dialog_started(&mut self) {
        self.dialog_open = true;
        self.blur_check_at = None;
    }

    pub fn dialog_finished(&mut self, now: Instant) {
        self.dialog_open = false;
        self.ignore_blur_until = Some(now + DIALOG_GRACE);
    }

    /// Ist die Prüfung fällig, und was folgt daraus? `foreground`: das Popover (oder sein Dialog) hat den Fokus.
    pub fn check(&mut self, now: Instant, foreground: bool) -> Option<BlurAction> {
        if foreground {
            self.seen_foreground = true;
        }
        if self.blur_check_at.is_none_or(|t| now < t) {
            return None;
        }
        self.blur_check_at = None;
        if self.dialog_open || foreground || self.ignore_blur_until.is_some_and(|t| now < t) {
            return Some(BlurAction::Keep);
        }
        if self.invalid {
            return Some(BlurAction::ShowError);
        }
        self.closed_by_blur_at = Some(now);
        Some(BlurAction::Close)
    }

    /// Soll ein Klick aufs Zahnrad das Popover öffnen? Nicht, wenn es sich gerade wegen dieses Klicks geschlossen hat.
    pub fn may_open(&self, now: Instant) -> bool {
        self.closed_by_blur_at.is_none_or(|t| now.duration_since(t) >= REOPEN_GUARD)
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.blur_check_at
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Quiet,
    Due,
    Saved,
}

/// Zustand der Anzeige (ruhig, fällig, gespeichert). Die Zeit kommt von aussen, damit man es testen kann.
#[derive(Default)]
pub struct Prompt {
    state: State,
    saved_until: Option<Instant>,
}

impl Prompt {
    pub fn state(&self) -> State {
        self.state
    }

    pub fn due(&mut self) {
        self.state = State::Due;
        self.saved_until = None;
    }

    pub fn saved(&mut self, now: Instant) {
        self.state = State::Saved;
        self.saved_until = Some(now + SAVED_FOR);
    }

    /// Zurück auf ruhig (Später, Überspringen)
    pub fn quiet(&mut self) {
        self.state = State::Quiet;
        self.saved_until = None;
    }

    /// Gibt `true` zurück, wenn sich der Zustand geändert hat
    pub fn tick(&mut self, now: Instant) -> bool {
        let before = self.state;
        if self.saved_until.is_some_and(|t| now >= t) {
            self.quiet();
        }
        self.state != before
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.saved_until
    }
}

pub struct Recap {
    journal: Journal,
    bar: Bar,
    /// Tag, den die Leiste zeigt
    bar_date: NaiveDate,
    /// Heute, wie es die Leiste zuletzt gesehen hat (für den Tageswechsel über Nacht)
    seen_today: NaiveDate,
    week: Popup,
    week_date: NaiveDate,
    settings_win: Popup,
    popover: PopoverGuard,
    /// Detailkarte der Claude-Limits: ein Popover wie die Einstellungen
    limits_win: Popup,
    limits_guard: PopoverGuard,
    limits: LimitsWatcher,
    limits_view: Option<limits::View>,
    next_limits: Instant,
    reminder: ReminderWindow,
    gate: Gate,
    /// Der sichtbare Hinweis ist die Wochenreflexion
    hint_reflection: bool,
    prompt: Prompt,
    reflection_pending: bool,
    settings: Settings,
    schedule: Schedule,
    timer: Timer,
    /// Ort-Override des Tages, einmal pro Tag aus der Wochen-Datei gelesen
    marker: Option<(NaiveDate, Option<String>)>,
    next_poll: Instant,
    git: Option<GitConfig>,
    next_git: Instant,
    scanning: Option<Instant>,
    /// Der erste Scan nach dem Start oder einer Änderung der Git-Einstellungen löst keinen Hinweis aus
    git_seen: bool,
    proxy: EventLoopProxy<UserEvent>,
    hotkey: Hotkey,
}

impl Recap {
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Result<Self, String> {
        let settings = Settings::load();
        let bar = Bar::new(target, proxy.clone(), settings.aufklappen)?;
        let reminder = ReminderWindow::new(target, proxy.clone())?;
        let week_proxy = proxy.clone();
        let week = Popup::new(
            target,
            "Notify – Woche",
            include_str!("./ui/week.html"),
            WEEK_SIZE,
            20.0,
            move |body| {
                if let Some(msg) = parse_week_msg(&body) {
                    week_proxy.send_event(UserEvent::Week(msg)).ok();
                }
            },
        )?;
        let settings_proxy = proxy.clone();
        let settings_win = Popup::new(
            target,
            "Notify – Einstellungen",
            include_str!("./ui/settings.html"),
            SETTINGS_SIZE,
            20.0,
            move |body| {
                if let Some(msg) = parse_settings_msg(&body) {
                    settings_proxy.send_event(UserEvent::Settings(msg)).ok();
                }
            },
        )?;
        let limits_proxy = proxy.clone();
        let limits_win = Popup::new(
            target,
            "Notify – Claude-Limits",
            include_str!("./ui/limits.html"),
            LIMITS_SIZE,
            20.0,
            move |body| {
                if let Some(msg) = parse_limits_msg(&body) {
                    limits_proxy.send_event(UserEvent::Limits(msg)).ok();
                }
            },
        )?;
        let schedule = settings.schedule().unwrap_or_else(|e| {
            log::error(&format!("Einstellungen ungültig, Standardwerte aktiv: {e}"));
            Settings::default().schedule().expect("default settings are valid")
        });
        let git = match settings.git() {
            Ok(config) if config.enabled() => Some(config),
            Ok(_) => None,
            Err(e) => {
                log::error(&format!("Git-Einstellungen ungültig, Git bleibt aus: {e}"));
                None
            }
        };
        system::start_session_watcher();
        let mut hotkey = Hotkey::new(proxy.clone());
        hotkey.set(&settings.hotkey);

        let today = Local::now().date_naive();
        let mut recap = Self {
            journal: Journal::from_settings(&settings),
            bar,
            bar_date: today,
            seen_today: today,
            week,
            week_date: today,
            settings_win,
            popover: PopoverGuard::new(),
            limits_win,
            limits_guard: PopoverGuard::new(),
            limits: LimitsWatcher::new(limits::path()),
            limits_view: None,
            next_limits: Instant::now(),
            reminder,
            gate: Gate::default(),
            hint_reflection: false,
            prompt: Prompt::default(),
            reflection_pending: false,
            settings,
            timer: Timer::new(schedule.clone(), Local::now().naive_local()),
            schedule,
            marker: None,
            next_poll: Instant::now() + POLL,
            git,
            next_git: Instant::now(),
            scanning: None,
            git_seen: false,
            proxy,
            hotkey,
        };
        recap.bar.show_quietly();
        Ok(recap)
    }

    pub fn state(&self) -> State {
        self.prompt.state()
    }

    pub fn deadline(&self) -> Option<Instant> {
        let mut deadline = self.next_poll;
        if self.git.is_some() {
            deadline = deadline.min(self.next_git);
        }
        let poll = (self.settings_win.is_visible() || self.limits_win.is_visible())
            .then(|| Instant::now() + POPOVER_POLL);
        let limits = self.settings.claude_limits.then_some(self.next_limits);
        let pending = [
            self.prompt.deadline(),
            self.bar.deadline(),
            self.popover.deadline(),
            self.limits_guard.deadline(),
            poll,
            limits,
        ]
        .into_iter()
        .flatten();
        Some(pending.fold(deadline, Instant::min))
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        if now >= self.next_poll {
            self.next_poll = now + POLL;
            self.poll();
        }
        if self.git.is_some() && now >= self.next_git {
            self.start_scan();
        }
        self.prompt.tick(now);
        if self.settings.claude_limits && now >= self.next_limits {
            self.next_limits = now + LIMITS_POLL;
            self.poll_limits();
        }
        if self.bar.tick() {
            // die Pille ist eingerastet: Pfeilrichtung neu bestimmen, Popover neu verankern
            self.refresh_pill();
            self.anchor_popovers();
        }
        self.check_settings_blur(now);
        self.check_limits_blur(now);
    }

    /// Sucht in einem eigenen Thread nach neuen Commits. Das Ergebnis kommt als Event `Commits`.
    fn start_scan(&mut self) {
        let Some(config) = self.git.clone() else { return };
        let now = Instant::now();
        self.next_git = now + GIT_EVERY;
        if self.scanning.is_some_and(|s| now.duration_since(s) < GIT_STUCK) {
            return;
        }
        self.scanning = Some(now);

        let proxy = self.proxy.clone();
        let since = git::lookback_start(Utc::now());
        let spawned = std::thread::Builder::new().name("git-scan".into()).spawn(move || {
            let commits = git::scan(&config, since);
            proxy.send_event(UserEvent::Commits(commits)).ok();
        });
        if let Err(e) = spawned {
            log::warn(&format!("git-Suche nicht gestartet: {e}"));
            self.scanning = None;
        }
    }

    /// Schreibt neue Commits ins Journal (ohne Duplikate), aktualisiert die offenen Fenster
    /// und löst bei neuen Commits von heute den Hinweis aus
    pub fn ingest(&mut self, commits: Vec<Commit>) {
        self.scanning = None;
        let first_scan = !std::mem::replace(&mut self.git_seen, true);
        let today = Local::now().date_naive();
        let existing = self.journal.list(git::lookback_first_day(today), today);
        let (mut added, mut fresh) = (0, 0);
        for c in git::new_commits(commits, &existing) {
            match self.journal.add(Entry::commit(c.t, &c.repo, &c.hash, &c.text)) {
                Ok(_) => {
                    added += 1;
                    fresh += usize::from(c.t.with_timezone(&Local).date_naive() == today);
                }
                Err(e) => log::warn(&format!("Commit {} nicht gespeichert: {e}", c.hash)),
            }
        }
        if added > 0 {
            log::info(&format!("git: {added} neue Commits ins Journal geschrieben"));
            self.refresh_all();
        }
        if fresh > 0 && !first_scan {
            self.commit_hint(fresh);
        }
    }

    /// Der Ort-Override des Tages (`None`: es gilt der Wochenplan)
    fn marked(&mut self, date: NaiveDate) -> Option<String> {
        match &self.marker {
            Some((d, ort)) if *d == date => ort.clone(),
            _ => {
                let ort = self.journal.day_override(date);
                self.marker = Some((date, ort.clone()));
                ort
            }
        }
    }

    fn poll(&mut self) {
        let now = Local::now().naive_local();
        self.roll_over(now.date());
        let away = self.schedule.is_away(system::idle_time(), system::session_away());
        let marked = self.marked(now.date());
        if self.timer.tick(now, away, marked.as_deref()) {
            let week_done = !self.journal.texts(now.date()).is_empty();
            self.reflection_pending = reflection_wanted(&self.schedule, now, week_done);
            self.raise_hint("Eintrag fällig", 0, true);
            self.start_scan();
        }
    }

    /// Über Nacht rückt die Leiste mit, wenn sie den alten "heute" zeigte
    fn roll_over(&mut self, today: NaiveDate) {
        if today == self.seen_today {
            return;
        }
        if self.bar_date == self.seen_today {
            self.bar_date = today;
        }
        self.seen_today = today;
        self.marker = None;
        self.refresh_all();
    }

    /// Zeigt den kleinen Hinweis. Der Stunden-Check-in (`force`) kommt immer, ein Commit-Hinweis nur,
    /// wenn im letzten Intervall noch kein Hinweis kam. Hat man gerade die Leiste im Fokus, bleibt es beim Tray-Icon.
    fn raise_hint(&mut self, title: &str, commits: usize, force: bool) {
        let now = Instant::now();
        let interval = Duration::from_secs(u64::from(self.schedule.interval_minutes) * 60);
        if !force && !self.gate.allows(now, interval) {
            return;
        }
        self.prompt.due();
        self.gate.shown(now);
        if self.bar.is_focused() {
            return;
        }

        let local = Local::now();
        let today = local.date_naive();
        let entries = self.journal.list(git::lookback_first_day(today), today);
        self.hint_reflection = force && self.reflection_pending;
        let hint = if self.hint_reflection {
            reminder::reflection_hint()
        } else {
            let pending =
                git::pending_commits(&entries, today).into_iter().filter(|e| e.quelle == Quelle::Commit).count();
            reminder::checkin_hint(title, commits.max(pending), &entries, local.naive_local())
        };
        let avoid = self.bar.group_rect().filter(|_| self.bar.is_visible());
        self.reminder.show(&hint, self.schedule.snooze_minutes, avoid);
    }

    /// Neuer Commit von heute: Hinweis nur an Arbeitstagen in der Arbeitszeit und wenn man nicht weg ist
    fn commit_hint(&mut self, commits: usize) {
        if !self.settings.commit_erinnerung {
            return;
        }
        let now = Local::now().naive_local();
        let away = self.schedule.is_away(system::idle_time(), system::session_away());
        let marked = self.marked(now.date());
        if reminder::commit_hint_allowed(&self.schedule, now, marked.as_deref(), away) {
            self.raise_hint("Neuer Commit", commits, false);
        }
    }

    /// Menü "Woche exportieren" und "Letzte Woche exportieren"
    pub fn export(&self, last: bool) {
        match export::export_week(Local::now().date_naive(), last) {
            Ok(path) => system::reveal_in_explorer(&path),
            Err(e) => {
                log::error(&format!("Export fehlgeschlagen: {e}"));
                system::show_error("Export fehlgeschlagen", &e);
            }
        }
    }

    /// Menü "Heute ist ...": setzt den Ort von heute (ein Override, wenn er vom Wochenplan abweicht)
    pub fn set_day(&mut self, kind: DayKind) {
        let today = Local::now().date_naive();
        let Some(ort) = self.schedule.plan.first_of(kind) else {
            log::warn("Heute ist ...: es gibt keinen passenden Ort in den Einstellungen");
            return;
        };
        let name = ort.name.clone();
        match self.set_ort(today, &name) {
            Ok(()) => self.refresh_all(),
            Err(e) => log::warn(&e),
        }
    }

    /// Stellt den Ort eines Tages ein (ein Override, wenn er vom Wochenplan abweicht)
    fn set_ort(&mut self, date: NaiveDate, ort: &str) -> Result<(), String> {
        let over = self.schedule.plan.override_for(date, ort)?;
        self.journal.set_day_override(date, over.as_deref()).map_err(|e| format!("Ort nicht gespeichert: {e}"))?;
        self.day_changed(date);
        Ok(())
    }

    /// Nach einer Änderung am Ort eines Tages: ist heute kein Arbeitstag mehr, endet die Erinnerung
    fn day_changed(&mut self, date: NaiveDate) {
        let today = Local::now().date_naive();
        if date != today {
            return;
        }
        self.marker = None;
        let over = self.marked(today);
        if !checkins_allowed(today, over.as_deref(), &self.schedule) {
            self.reflection_pending = false;
            self.timer.answered();
            self.prompt.quiet();
            self.reminder.hide();
        }
    }

    /// Menüpunkt "Eintrag schreiben", Hotkey und "Jetzt eintragen": Leiste auf heute, ausgeklappt, Eingabe fokussiert
    pub fn open_entry(&mut self) {
        self.bar_date = Local::now().date_naive();
        self.bar.show_focused();
        self.refresh_bar();
        self.bar.focus_panel_input();
        self.anchor_popovers();
        self.start_scan();
    }

    /// Globales Tastenkürzel: einblenden und fokussieren, mit Fokus wieder ausblenden
    pub fn toggle_bar(&mut self) {
        if self.bar.is_focused() {
            self.hide_bar();
        } else {
            self.reminder.hide();
            self.open_entry();
        }
    }

    /// Blendet die Leiste aus (Esc, Hotkey); das Einstellungs-Popover geht mit
    fn hide_bar(&mut self) {
        self.close_settings();
        self.close_limits();
        self.bar.hide();
    }

    pub fn handle_window_event(&mut self, id: WindowId, event: &WindowEvent) {
        self.bar.handle_event(id, event);
        self.week.handle_event(id, event);
        self.settings_win.handle_event(id, event);
        self.limits_win.handle_event(id, event);
        self.reminder.handle_event(id, event);
        if id == self.settings_win.id() {
            match event {
                WindowEvent::Focused(false) if self.settings_win.is_visible() => {
                    self.popover.focus_lost(Instant::now());
                }
                WindowEvent::Focused(true) => self.popover.focus_gained(),
                _ => {}
            }
        }
        if id == self.limits_win.id() {
            match event {
                WindowEvent::Focused(false) if self.limits_win.is_visible() => {
                    self.limits_guard.focus_lost(Instant::now());
                }
                WindowEvent::Focused(true) => self.limits_guard.focus_gained(),
                _ => {}
            }
        }
        if !matches!(event, WindowEvent::CloseRequested) {
            return;
        }
        if self.bar.owns(id) {
            self.hide_bar();
        } else if id == self.week.id() {
            self.week.hide();
        } else if id == self.settings_win.id() {
            self.close_settings();
        } else if id == self.limits_win.id() {
            self.close_limits();
        } else if id == self.reminder.id() {
            self.reminder.hide();
        }
    }

    // --- Leiste ---

    fn bar_data(&self) -> serde_json::Value {
        let entries = self.journal.day(self.bar_date);
        let over = self.journal.day_override(self.bar_date);
        let mut data = view::bar_view(
            self.bar_date,
            Local::now().date_naive(),
            &entries,
            over.as_deref(),
            &self.schedule.plan,
            self.bar.is_collapsed(),
        );
        data["expanded"] = serde_json::json!(!self.bar.is_collapsed());
        data["dir"] = serde_json::json!(if self.bar.direction() == layout::Dir::Down { "down" } else { "up" });
        data["settings_open"] = serde_json::json!(self.settings_win.is_visible());
        limits::add_to_pill_data(&mut data, &self.limits_view, &self.settings.ring_colors());
        data["limits_open"] = serde_json::json!(self.limits_win.is_visible());
        data
    }

    fn refresh_bar(&self) {
        let data = self.bar_data();
        self.bar.script_panel(&format!("onDay({data})"));
        self.bar.script_pill(&format!("onPill({data})"));
    }

    fn refresh_pill(&self) {
        let data = self.bar_data();
        self.bar.script_pill(&format!("onPill({data})"));
    }

    fn refresh_all(&self) {
        self.refresh_bar();
        if self.week.is_visible() {
            self.refresh_week();
        }
    }

    pub fn handle_bar(&mut self, msg: BarMsg) {
        let today = Local::now().date_naive();
        let result = match msg {
            BarMsg::Ready => Ok(()),
            BarMsg::Nav { delta } => {
                self.bar_date = view::shift(self.bar_date, delta, today);
                Ok(())
            }
            BarMsg::Today => {
                self.bar_date = today;
                Ok(())
            }
            BarMsg::SetOrt { ort } => self.set_ort(self.bar_date, &ort),
            BarMsg::Add { text, hours } => self.add_entry(&text, &hours),
            BarMsg::Update { id, text } => self.update_entry(&id, &text),
            BarMsg::Delete { id } => self.delete_entry(&id),
            BarMsg::Rest => self.book_rest(),
            BarMsg::Collapse { collapsed } => {
                self.bar.set_collapsed(collapsed);
                self.anchor_popovers();
                Ok(())
            }
            BarMsg::Settings => {
                self.toggle_settings();
                return;
            }
            BarMsg::Limits => {
                self.toggle_limits();
                return;
            }
            BarMsg::Week => {
                self.open_week();
                Ok(())
            }
            BarMsg::Hide => {
                self.hide_bar();
                return;
            }
            BarMsg::Drag => {
                self.bar.drag();
                return;
            }
        };
        match result {
            Ok(()) => self.refresh_all(),
            Err(e) => {
                log::warn(&format!("Leiste: {e}"));
                self.bar.script_panel(&format!("onError({})", js_string(&e)));
            }
        }
    }

    fn add_entry(&mut self, text: &str, hours: &str) -> Result<(), String> {
        let (text, hours) = view::parse_entry_input(text, hours)?;
        let now = Local::now();
        let t = view::entry_time(self.bar_date, now);
        self.journal.add(Entry::manuell(t, &text, hours))?;
        self.bar.script_panel("onAdded()");
        if self.bar_date == now.date_naive() {
            self.timer.answered();
            self.prompt.saved(Instant::now());
            if !self.hint_reflection {
                self.reminder.hide();
            }
        }
        Ok(())
    }

    /// Ändert den Text eines Eintrags. Steht am Ende eine Dauer (`... 2.4`), ersetzt sie die bisherige.
    fn update_entry(&self, id: &str, text: &str) -> Result<(), String> {
        let (text, hours) = split_duration(text);
        let stunden = if hours > 0.0 {
            hours
        } else {
            self.journal.day(self.bar_date).iter().find(|e| e.id == id).map_or(0.0, |e| e.stunden)
        };
        self.journal.update(id, Update { text, stunden }).map(|_| ())
    }

    /// Löscht eine eigene Notiz. Commits bleiben, sonst importiert Git sie neu.
    fn delete_entry(&self, id: &str) -> Result<(), String> {
        let entries = self.journal.day(self.bar_date);
        match entries.iter().find(|e| e.id == id) {
            Some(e) if e.quelle == Quelle::Manuell => self.journal.remove(id),
            Some(_) => Err("Commits lassen sich nicht löschen".to_string()),
            None => Err("Eintrag nicht gefunden".to_string()),
        }
    }

    /// Knopf "Rest auf letzte Zeile"
    fn book_rest(&self) -> Result<(), String> {
        let over = self.journal.day_override(self.bar_date);
        let soll = self.schedule.plan.resolve(self.bar_date, over.as_deref()).map_or(0, |o| o.soll_tenths());
        export::rest_buchen(&self.journal, self.bar_date, soll).map(|_| ())
    }

    // --- Hinweis ---

    pub fn handle_reminder(&mut self, msg: ReminderMsg) {
        self.reminder.hide();
        match msg {
            ReminderMsg::Now if self.hint_reflection => self.open_week(),
            ReminderMsg::Now => self.open_entry(),
            ReminderMsg::Snooze => {
                self.timer.snooze(Local::now().naive_local());
                self.prompt.quiet();
            }
            ReminderMsg::Close => {}
        }
    }

    // --- Woche ---

    /// Menüpunkt "Woche"
    pub fn open_week(&mut self) {
        self.week_date = Local::now().date_naive();
        self.refresh_week();
        if !self.week.is_visible() {
            self.place_week();
        }
        self.week.show(true);
    }

    /// Die Wochenansicht erscheint neben der Leiste, sonst in der Mitte des Bildschirms
    fn place_week(&self) {
        let (w, h) = (WEEK_SIZE.0 * self.week.scale(), WEEK_SIZE.1 * self.week.scale());
        let size = (w.round() as i32, h.round() as i32);
        let Some(work) = self.bar.work_area().or_else(|| self.week.work_area()) else { return };
        let pos = match self.bar.group_rect().filter(|_| self.bar.is_visible()) {
            Some(bar) => layout::anchor_popover(bar, size, work, self.gap_px()),
            None => (work.x + (work.w - size.0) / 2, work.y + (work.h - size.1) / 2),
        };
        self.week.set_size(WEEK_SIZE);
        self.week.move_to(pos.0, pos.1);
    }

    /// Abstand zwischen Leiste und Popover in Pixeln
    fn gap_px(&self) -> i32 {
        (layout::GAP * self.settings_win.scale()).round() as i32
    }

    fn export_config(&self) -> ExportConfig {
        self.settings
            .export()
            .unwrap_or_else(|_| Settings::default().export().expect("default settings are valid"))
    }

    fn refresh_week(&self) {
        let file = self.journal.week_file(self.week_date);
        let data = view::week_view(self.week_date, Local::now().date_naive(), &file, &self.export_config());
        self.week.script(&format!("onWeek({data})"));
    }

    pub fn handle_week(&mut self, msg: WeekMsg) {
        let result = match msg {
            WeekMsg::Close => {
                self.week.hide();
                return;
            }
            WeekMsg::Drag => {
                self.week.drag();
                return;
            }
            WeekMsg::Nav { delta } => {
                self.week_date = view::shift_weeks(self.week_date, delta);
                Ok(())
            }
            WeekMsg::ThisWeek => {
                self.week_date = Local::now().date_naive();
                Ok(())
            }
            WeekMsg::SetOrt { date, ort } => self.set_ort(date, &ort),
            WeekMsg::Whole { kind } => self.set_whole_week(kind),
            WeekMsg::SetTexts { rueckblick, reflexion, stimmung } => {
                self.set_week_texts(WeekTexts { rueckblick, reflexion, stimmung })
            }
            WeekMsg::Export => match export::export_week_of(self.week_date) {
                Ok(path) => {
                    system::reveal_in_explorer(&path);
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.week.script(&format!("onExported({})", js_string(&name)));
                    return;
                }
                Err(e) => {
                    log::error(&format!("Export fehlgeschlagen: {e}"));
                    Err(e)
                }
            },
        };
        match result {
            Ok(()) => self.refresh_all(),
            Err(e) => {
                log::warn(&format!("Woche: {e}"));
                self.week.script(&format!("onError({})", js_string(&e)));
            }
        }
    }

    /// "Ganze Woche = Ferien/üK": Montag bis Freitag auf den ersten Ort der Art, oder zurück auf den Wochenplan
    fn set_whole_week(&mut self, kind: WholeWeek) -> Result<(), String> {
        let ort = match kind {
            WholeWeek::Plan => String::new(),
            WholeWeek::Ferien | WholeWeek::Uek => {
                let art = if kind == WholeWeek::Uek { DayKind::Uek } else { DayKind::Ferien };
                let ort = self.schedule.plan.first_of(art).ok_or("Es gibt keinen passenden Ort in den Einstellungen")?;
                ort.name.clone()
            }
        };
        let (monday, _) = week_bounds(self.week_date);
        for n in 0..5u64 {
            let day = monday.checked_add_days(Days::new(n)).unwrap_or(monday);
            let over = self.schedule.plan.override_for(day, &ort)?;
            self.journal.set_day_override(day, over.as_deref()).map_err(|e| format!("Ort nicht gespeichert: {e}"))?;
            self.day_changed(day);
        }
        Ok(())
    }

    fn set_week_texts(&mut self, texts: WeekTexts) -> Result<(), String> {
        let saved = self.journal.set_texts(self.week_date, texts)?;
        // Die Wochenreflexion ist geschrieben: kein Hinweis mehr dafür
        let this_week = iso_week(self.week_date) == iso_week(Local::now().date_naive());
        if this_week && self.reflection_pending && !saved.is_empty() {
            self.reflection_pending = false;
            self.timer.answered();
            self.prompt.saved(Instant::now());
            if self.hint_reflection {
                self.reminder.hide();
            }
        }
        Ok(())
    }

    // --- Einstellungen ---

    /// Menüpunkt "Einstellungen ...": öffnet das Popover an der Leiste (ist es schon offen, bekommt es den Fokus)
    pub fn open_settings(&mut self) {
        if self.settings_win.is_visible() {
            self.settings_win.show(true);
        } else {
            self.show_settings();
        }
    }

    /// Zahnrad: öffnet das Popover oder schliesst es wieder
    fn toggle_settings(&mut self) {
        if self.settings_win.is_visible() {
            self.close_settings();
        } else if self.popover.may_open(Instant::now()) {
            self.show_settings();
        }
    }

    fn show_settings(&mut self) {
        if !self.bar.is_visible() {
            self.bar.show_quietly();
            self.refresh_bar();
        }
        self.close_limits();
        self.popover.reset();
        let (year, kw) = iso_week(Local::now().date_naive());
        let data = serde_json::json!({
            "settings": self.settings,
            "autostart": system::autostart_enabled(),
            "week": { "year": year, "kw": kw },
        });
        self.settings_win.script(&format!("onSettings({data})"));
        self.settings_win.set_size(self.popover.size());
        self.place_settings();
        self.settings_win.show(true);
        self.refresh_pill();
    }

    fn close_settings(&mut self) {
        if self.settings_win.is_visible() {
            self.settings_win.hide();
            self.refresh_pill();
        }
        self.popover.focus_gained();
    }

    /// Setzt das Popover neben die Leiste: ganz auf den Monitor, ohne Pille oder Panel zu verdecken
    fn anchor_popovers(&self) {
        if self.settings_win.is_visible() {
            self.place_settings();
        }
        if self.limits_win.is_visible() {
            self.place_limits();
        }
    }

    fn place_settings(&self) {
        let Some(bar) = self.bar.group_rect() else { return };
        let Some(work) = self.bar.work_area() else { return };
        let size = self.settings_win.size_px();
        let old = self.settings_win.position();
        let (x, mut y) = layout::anchor_popover(bar, size, work, self.gap_px());
        // Beim Wechsel der Höhe bleibt die Oberkante, wenn es passt
        if let Some((_, oy)) = old.filter(|&(ox, _)| ox == x) {
            if self.settings_win.is_visible() && oy >= work.y && oy + size.1 <= work.bottom() {
                y = oy;
            }
        }
        self.settings_win.move_to(x, y);
    }

    /// Fokus ist weg: schliesst das Popover, ausser ein Dialog ist offen oder ein ungespeicherter Wert ist ungültig
    fn check_settings_blur(&mut self, now: Instant) {
        if !self.settings_win.is_visible() {
            self.popover.focus_gained();
            return;
        }
        let foreground = self.settings_win.is_foreground();
        if !self.popover.dialog_open() {
            self.popover.observe(now, foreground);
        }
        match self.popover.check(now, foreground) {
            Some(BlurAction::Close) => self.close_settings(),
            Some(BlurAction::ShowError) => self.settings_win.script("showInvalid()"),
            Some(BlurAction::Keep) | None => {}
        }
    }

    // --- Claude-Limits ---

    /// Schaut nach der Datei und bringt Pille und offene Karte auf den Stand
    fn poll_limits(&mut self) {
        self.limits.poll();
        let now = Utc::now().timestamp();
        let view = self.limits.current().and_then(|l| l.view(now, &limits::local_offset, self.settings.ring_thresholds()));
        if view != self.limits_view {
            self.limits_view = view;
            self.refresh_pill();
        }
        if self.limits_win.is_visible() {
            self.push_limits_card(now);
        }
    }

    fn push_limits_card(&self, now: i64) {
        let data = limits::card_data(&self.limits_view, &self.settings.ring_colors(), now);
        self.limits_win.script(&format!("onLimits({data})"));
    }

    /// Schalter in den Einstellungen: aus = Ort-Icon, Karte zu, kein Polling; an = sofort lesen
    fn apply_limits_switch(&mut self) {
        if self.settings.claude_limits {
            self.limits = LimitsWatcher::new(limits::path());
            self.next_limits = Instant::now();
        } else {
            self.limits_view = None;
            self.close_limits();
        }
    }

    /// Klick auf den Ring: öffnet die Karte oder schliesst sie wieder
    fn toggle_limits(&mut self) {
        if self.limits_win.is_visible() {
            self.close_limits();
        } else if self.limits_guard.may_open(Instant::now()) && self.limits_view.is_some() {
            self.show_limits();
        }
    }

    fn show_limits(&mut self) {
        if !self.bar.is_visible() {
            self.bar.show_quietly();
        }
        self.close_settings();
        self.limits_guard.reset();
        self.poll_limits();
        self.push_limits_card(Utc::now().timestamp());
        self.limits_win.set_size(LIMITS_SIZE);
        self.place_limits();
        self.limits_win.show(true);
        self.refresh_pill();
    }

    fn close_limits(&mut self) {
        if self.limits_win.is_visible() {
            self.limits_win.hide();
            self.refresh_pill();
        }
        self.limits_guard.focus_gained();
    }

    fn place_limits(&self) {
        let Some(bar) = self.bar.group_rect() else { return };
        let Some(work) = self.bar.work_area() else { return };
        let pos = layout::anchor_popover(bar, self.limits_win.size_px(), work, self.gap_px());
        self.limits_win.move_to(pos.0, pos.1);
    }

    /// Fokus ist weg (Klick ausserhalb): schliesst die Karte
    fn check_limits_blur(&mut self, now: Instant) {
        if !self.limits_win.is_visible() {
            self.limits_guard.focus_gained();
            return;
        }
        let foreground = self.limits_win.is_foreground();
        self.limits_guard.observe(now, foreground);
        if self.limits_guard.check(now, foreground) == Some(BlurAction::Close) {
            self.close_limits();
        }
    }

    pub fn handle_limits(&mut self, msg: LimitsMsg) {
        match msg {
            LimitsMsg::Close => self.close_limits(),
        }
    }

    /// Ergebnis des Datei- oder Ordnerdialogs
    pub fn picked(&mut self, field: String, path: Option<String>) {
        self.popover.dialog_finished(Instant::now());
        if !self.settings_win.is_visible() {
            return;
        }
        self.settings_win.show(true);
        if let Some(path) = path {
            self.settings_win.script(&format!("onPicked({}, {})", js_string(&field), js_string(&path)));
        }
    }

    fn pick(&mut self, field: String, kind: PickKind) {
        if self.popover.dialog_open() {
            return;
        }
        self.popover.dialog_started();
        let owner = self.settings_win.hwnd();
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new().name("pick-dialog".into()).spawn(move || {
            let path = match kind {
                PickKind::Folder => system::pick_folder(owner, "Ordner wählen"),
                PickKind::Docx => system::pick_docx(owner, "Word-Vorlage wählen"),
            };
            proxy.send_event(UserEvent::Picked { field, path: path.map(|p| p.display().to_string()) }).ok();
        });
        if let Err(e) = spawned {
            log::warn(&format!("Dialog nicht gestartet: {e}"));
            self.popover.dialog_finished(Instant::now());
        }
    }

    pub fn handle_settings(&mut self, msg: SettingsMsg) {
        match msg {
            SettingsMsg::Close => self.close_settings(),
            SettingsMsg::Invalid { invalid } => self.popover.set_invalid(invalid),
            SettingsMsg::Tall { tall } => {
                if self.popover.set_tall(tall) {
                    self.settings_win.set_size(self.popover.size());
                    self.anchor_popovers();
                }
            }
            SettingsMsg::Pick { field, kind } => self.pick(field, kind),
            SettingsMsg::Save(input) => match self.save_settings(*input) {
                Ok(()) => self.close_settings(),
                Err(e) => {
                    log::warn(&format!("Einstellungen nicht gespeichert: {e}"));
                    self.settings_win.script(&format!("onError({})", js_string(&e)));
                }
            },
            SettingsMsg::SaveTemplate => match export::save_default_template() {
                Ok(path) => {
                    system::reveal_in_explorer(&path);
                    let name = path.display().to_string();
                    self.settings_win.script(&format!("onTemplateSaved({})", js_string(&name)));
                }
                Err(e) => self.settings_win.script(&format!("onError({})", js_string(&e))),
            },
        }
    }

    fn save_settings(&mut self, input: SettingsInput) -> Result<(), String> {
        let (settings, autostart) = input.into_settings(&self.settings)?;
        let schedule = settings.schedule()?;
        let git = settings.git()?;
        check_git_folders(&git)?;
        settings.save()?;
        for (name, color) in [
            ("5 Stunden", &settings.ring_five_hour),
            ("Woche", &settings.ring_seven_day),
            ("Kontext", &settings.ring_context),
            ("Warnung", &settings.ring_warn),
            ("Kritisch", &settings.ring_crit),
        ] {
            if limits::hardly_visible(color) {
                log::info(&format!("Ringfarbe {name} ({color}) ist gegen die Spur kaum sichtbar"));
            }
        }

        self.timer = Timer::new(schedule.clone(), Local::now().naive_local());
        self.schedule = schedule;
        if settings.git_folders != self.settings.git_folders || settings.git_emails != self.settings.git_emails {
            self.git_seen = false;
        }
        self.git = git.enabled().then_some(git);
        self.next_git = Instant::now();
        if settings.hotkey != self.settings.hotkey {
            self.hotkey.set(&settings.hotkey);
        }
        // Ein anderer Journal-Ordner oder Wochenplan gilt sofort
        self.journal = Journal::from_settings(&settings);
        self.marker = None;
        self.bar.set_pref(settings.aufklappen);
        let limits_toggled = settings.claude_limits != self.settings.claude_limits;
        let thresholds_changed = settings.ring_thresholds() != self.settings.ring_thresholds();
        self.settings = settings;
        if thresholds_changed {
            // Die Warnstufen stecken in der Ansicht, also neu berechnen
            self.limits_view = None;
            self.poll_limits();
        }
        if limits_toggled {
            self.apply_limits_switch();
        }
        // Pille (in refresh_all) und eine offene Karte bekommen die Farben sofort
        if self.limits_win.is_visible() {
            self.push_limits_card(Utc::now().timestamp());
        }
        self.refresh_all();

        system::set_autostart(autostart).map_err(|e| {
            format!("Die Einstellungen sind gespeichert, aber der Autostart konnte nicht geändert werden: {e}")
        })
    }

    /// Menüpunkt "Journal-Ordner öffnen"
    pub fn open_journal_folder(&self) {
        let dir = self.journal.dir();
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::warn(&format!("Journal-Ordner nicht erstellbar: {e}"));
        }
        system::open_folder(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popover_closes_on_focus_loss_after_a_short_delay() {
        let t0 = Instant::now();
        let mut g = PopoverGuard::new();
        // ohne Fokusverlust passiert nichts
        assert_eq!(g.check(t0, false), None);
        assert_eq!(g.deadline(), None);
        g.focus_lost(t0);
        assert_eq!(g.deadline(), Some(t0 + BLUR_DELAY));
        // zu früh
        assert_eq!(g.check(t0 + BLUR_DELAY - Duration::from_millis(1), false), None);
        assert_eq!(g.check(t0 + BLUR_DELAY, false), Some(BlurAction::Close));
        // die Prüfung ist verbraucht
        assert_eq!(g.deadline(), None);
        assert_eq!(g.check(t0 + BLUR_DELAY * 2, false), None);
    }

    #[test]
    fn popover_stays_when_it_got_the_focus_back() {
        let t0 = Instant::now();
        let mut g = PopoverGuard::new();
        g.focus_lost(t0);
        g.focus_gained();
        assert_eq!(g.check(t0 + BLUR_DELAY, false), None);
        // oder wenn Windows meldet, dass das Fenster (oder sein Dialog) noch vorne ist
        g.focus_lost(t0);
        assert_eq!(g.check(t0 + BLUR_DELAY, true), Some(BlurAction::Keep));
    }

    #[test]
    fn popover_polling_notices_a_lost_focus_without_an_event() {
        let t0 = Instant::now();
        let mut g = PopoverGuard::new();
        // noch nie im Vordergrund gewesen: kein Schliessen (es hat den Fokus vielleicht nie bekommen)
        g.observe(t0, false);
        assert_eq!(g.deadline(), None);
        // im Vordergrund, dann weg
        g.observe(t0, true);
        g.observe(t0 + Duration::from_millis(250), false);
        assert_eq!(g.deadline(), Some(t0 + Duration::from_millis(250) + BLUR_DELAY));
        assert_eq!(g.check(t0 + Duration::from_millis(250) + BLUR_DELAY, false), Some(BlurAction::Close));
        // ein erneuter Blick setzt die Prüfung nicht dauernd zurück
        let mut g = PopoverGuard::new();
        g.observe(t0, true);
        g.observe(t0 + Duration::from_millis(100), false);
        let due = g.deadline();
        g.observe(t0 + Duration::from_millis(200), false);
        assert_eq!(g.deadline(), due);
        // kommt der Fokus zurück, entfällt die Prüfung
        g.observe(t0 + Duration::from_millis(220), true);
        assert_eq!(g.deadline(), None);
    }

    #[test]
    fn popover_stays_open_with_an_invalid_value() {
        let t0 = Instant::now();
        let mut g = PopoverGuard::new();
        g.set_invalid(true);
        g.focus_lost(t0);
        assert_eq!(g.check(t0 + BLUR_DELAY, false), Some(BlurAction::ShowError));
        // ist der Wert wieder gültig, schliesst es sich
        g.set_invalid(false);
        g.focus_lost(t0 + BLUR_DELAY);
        assert_eq!(g.check(t0 + BLUR_DELAY * 2, false), Some(BlurAction::Close));
    }

    #[test]
    fn popover_stays_open_while_a_dialog_is_open_and_shortly_after() {
        let t0 = Instant::now();
        let mut g = PopoverGuard::new();
        g.dialog_started();
        assert!(g.dialog_open());
        g.focus_lost(t0);
        // Fokusverlust, solange der Dialog offen ist (auch ein verspäteter Event): nie schliessen
        assert_eq!(g.check(t0 + BLUR_DELAY, false), Some(BlurAction::Keep));
        g.focus_lost(t0);
        assert_eq!(g.check(t0 + Duration::from_secs(5), false), Some(BlurAction::Keep));
        // nach dem Dialog gilt eine kurze Schonfrist
        let done = t0 + Duration::from_secs(6);
        g.dialog_finished(done);
        assert!(!g.dialog_open());
        g.focus_lost(done);
        assert_eq!(g.check(done + BLUR_DELAY, false), Some(BlurAction::Keep));
        g.focus_lost(done + DIALOG_GRACE);
        assert_eq!(g.check(done + DIALOG_GRACE + BLUR_DELAY, false), Some(BlurAction::Close));
    }

    #[test]
    fn gear_click_right_after_a_blur_close_does_not_reopen() {
        let t0 = Instant::now();
        let mut g = PopoverGuard::new();
        assert!(g.may_open(t0));
        g.focus_lost(t0);
        assert_eq!(g.check(t0 + BLUR_DELAY, false), Some(BlurAction::Close));
        // der Klick auf das Zahnrad, der den Fokus genommen hat, kommt kurz danach an
        assert!(!g.may_open(t0 + BLUR_DELAY + Duration::from_millis(50)));
        assert!(g.may_open(t0 + BLUR_DELAY + REOPEN_GUARD));
        // das Zurücksetzen beim Öffnen hebt die Sperre nicht auf
        g.reset();
        assert!(!g.may_open(t0 + BLUR_DELAY + Duration::from_millis(50)));
    }

    #[test]
    fn popover_height_follows_the_tab() {
        let mut g = PopoverGuard::new();
        assert_eq!(g.size(), (760.0, 620.0));
        assert!(g.set_tall(true));
        assert_eq!(g.size(), (760.0, 700.0));
        assert!(!g.set_tall(true), "keine Änderung");
        assert!(g.set_tall(false));
        assert_eq!(g.size(), (760.0, 620.0));
        // zurücksetzen beim Öffnen: wieder die kurze Höhe
        g.set_tall(true);
        g.reset();
        assert_eq!(g.size(), (760.0, 620.0));
    }

    #[test]
    fn starts_quiet_without_deadline() {
        let mut p = Prompt::default();
        assert_eq!(p.state(), State::Quiet);
        assert_eq!(p.deadline(), None);
        assert!(!p.tick(Instant::now()));
    }

    #[test]
    fn due_until_saved_then_quiet() {
        let t0 = Instant::now();
        let mut p = Prompt::default();
        p.due();
        assert_eq!(p.state(), State::Due);
        assert_eq!(p.deadline(), None);

        p.saved(t0);
        assert_eq!(p.state(), State::Saved);
        assert_eq!(p.deadline(), Some(t0 + SAVED_FOR));
        assert!(!p.tick(t0 + SAVED_FOR - Duration::from_secs(1)));
        assert!(p.tick(t0 + SAVED_FOR));
        assert_eq!(p.state(), State::Quiet);
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn due_overrides_saved_and_quiet_clears_it() {
        let t0 = Instant::now();
        let mut p = Prompt::default();
        p.saved(t0);
        p.due();
        assert_eq!(p.deadline(), None);
        assert!(!p.tick(t0 + SAVED_FOR));
        assert_eq!(p.state(), State::Due);
        p.quiet();
        assert_eq!(p.state(), State::Quiet);
    }
}
