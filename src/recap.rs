use crate::export;
use crate::git::{self, Commit, GitConfig};
use crate::journal::{DayKind, Entry, Journal, Update};
use crate::log;
use crate::settings::{Settings, SettingsInput, check_git_folders};
use crate::system;
use crate::timer::{Schedule, Timer, checkins_allowed, reflection_wanted};
use crate::today;
use crate::tray::UserEvent;
use crate::widget::js_string;
use crate::windows::{
    EntryMsg, EntryWindow, Mode, Panel, SettingsMsg, TodayMsg, parse_settings_msg, parse_today_msg,
};
use chrono::{Local, NaiveDate, Utc};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::time::{Duration, Instant};
use tao::dpi::LogicalSize;
use tao::event::WindowEvent;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::WindowId;

const POLL: Duration = Duration::from_secs(15);
const GIT_EVERY: Duration = Duration::from_secs(5 * 60);
const GIT_STUCK: Duration = Duration::from_secs(5 * 60);
const SAVED_FOR: Duration = Duration::from_secs(10);

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

const SOURCE_CHECKIN: &str = "checkin";
const SOURCE_QUICK: &str = "quick";

pub struct Recap {
    journal: Journal,
    window: EntryWindow,
    prompt: Prompt,
    source: &'static str,
    reflection_pending: bool,
    settings: Settings,
    today: Panel,
    today_date: NaiveDate,
    settings_win: Panel,
    schedule: Schedule,
    timer: Timer,
    marker: Option<(NaiveDate, Option<DayKind>)>,
    next_poll: Instant,
    git: Option<GitConfig>,
    next_git: Instant,
    scanning: Option<Instant>,
    proxy: EventLoopProxy<UserEvent>,
    _hotkey: Option<GlobalHotKeyManager>,
}

impl Recap {
    pub fn new(
        target: &EventLoopWindowTarget<UserEvent>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Result<Self, String> {
        let window = EntryWindow::new(target, proxy.clone())?;
        let today_proxy = proxy.clone();
        let today = Panel::new(
            target,
            "Heutige Einträge",
            include_str!("./ui/today.html"),
            LogicalSize::new(520.0, 640.0),
            move |body| {
                if let Some(msg) = parse_today_msg(&body) {
                    today_proxy.send_event(UserEvent::Today(msg)).ok();
                }
            },
        )?;
        let settings_proxy = proxy.clone();
        let settings_win = Panel::new(
            target,
            "Einstellungen",
            include_str!("./ui/settings.html"),
            LogicalSize::new(620.0, 760.0),
            move |body| {
                if let Some(msg) = parse_settings_msg(&body) {
                    settings_proxy.send_event(UserEvent::Settings(msg)).ok();
                }
            },
        )?;
        let settings = Settings::load();
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
        Ok(Self {
            journal: Journal::open(),
            window,
            prompt: Prompt::default(),
            source: SOURCE_CHECKIN,
            reflection_pending: false,
            settings,
            today,
            today_date: Local::now().date_naive(),
            settings_win,
            timer: Timer::new(schedule.clone(), Local::now().naive_local()),
            schedule,
            marker: None,
            next_poll: Instant::now() + POLL,
            git,
            next_git: Instant::now(),
            scanning: None,
            _hotkey: register_hotkey(proxy.clone()),
            proxy,
        })
    }

    pub fn state(&self) -> State {
        self.prompt.state()
    }

    pub fn deadline(&self) -> Option<Instant> {
        let mut deadline = self.next_poll;
        if self.git.is_some() {
            deadline = deadline.min(self.next_git);
        }
        Some(self.prompt.deadline().map_or(deadline, |d| d.min(deadline)))
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

    /// Schreibt neue Commits ins Journal (ohne Duplikate) und aktualisiert das offene Fenster
    pub fn ingest(&mut self, commits: Vec<Commit>) {
        self.scanning = None;
        let today = Local::now().date_naive();
        let existing = self.journal.list(git::lookback_first_day(today), today);
        let mut added = 0;
        for c in git::new_commits(commits, &existing) {
            match self.journal.add(Entry::commit(c.t, &c.repo, &c.hash, &c.text)) {
                Ok(_) => added += 1,
                Err(e) => log::warn(&format!("Commit {} nicht gespeichert: {e}", c.hash)),
            }
        }
        if added > 0 {
            log::info(&format!("git: {added} neue Commits ins Journal geschrieben"));
            if self.window.is_visible() {
                self.refresh_commits();
            }
            self.refresh_today_if_visible();
        }
    }

    fn refresh_commits(&self) {
        let today = Local::now().date_naive();
        let entries = self.journal.list(git::lookback_first_day(today), today);
        let items: Vec<(String, String)> = git::pending_commits(&entries, today)
            .into_iter()
            .filter_map(|e| match e {
                Entry::Commit { repo, text, .. } => Some((repo.clone(), text.clone())),
                _ => None,
            })
            .collect();
        self.window.set_commits(&items);
    }

    /// Die letzte Tagesmarkierung, einmal pro Tag aus dem Journal gelesen
    fn marked(&mut self, date: NaiveDate) -> Option<DayKind> {
        match self.marker {
            Some((d, kind)) if d == date => kind,
            _ => {
                let kind = self.journal.day_kind(date);
                self.marker = Some((date, kind));
                kind
            }
        }
    }

    fn poll(&mut self) {
        let now = Local::now().naive_local();
        let away = self.schedule.is_away(system::idle_time(), system::session_away());
        let marked = self.marked(now.date());
        if self.timer.tick(now, away, marked) {
            let week_done = self
                .journal
                .week(now.date())
                .iter()
                .any(|e| matches!(e, Entry::Reflection { .. }));
            self.reflection_pending = reflection_wanted(&self.schedule, now, week_done);
            self.prompt.due();
            self.start_scan();
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

    /// Menü "Heute ist ...": schreibt die Tagesmarkierung
    pub fn set_day(&mut self, kind: DayKind) {
        if let Err(e) = self.journal.add(Entry::day(Utc::now(), kind)) {
            log::warn(&format!("Tagesmarkierung nicht gespeichert: {e}"));
            return;
        }
        let today = Local::now().date_naive();
        self.marker = Some((today, Some(kind)));
        self.refresh_today_if_visible();
        if !checkins_allowed(today, Some(kind), &self.schedule) {
            self.reflection_pending = false;
            self.timer.answered();
            self.prompt.quiet();
        }
    }

    /// Menüpunkt "Eintrag jetzt schreiben". Ist gerade die Wochenreflexion fällig, kommt sie statt der Notiz.
    pub fn open_checkin(&mut self) {
        let reflection = self.prompt.state() == State::Due && self.reflection_pending;
        let mode = if reflection { Mode::Reflection } else { Mode::Note };
        self.open(SOURCE_CHECKIN, mode, false);
    }

    /// Globales Tastenkürzel
    pub fn open_quick(&mut self) {
        self.open(SOURCE_QUICK, Mode::Note, false);
    }

    /// Menüpunkt "Wochenreflexion schreiben"
    pub fn open_reflection(&mut self) {
        self.open(SOURCE_CHECKIN, Mode::Reflection, true);
    }

    /// `explicit`: Ein schon offenes Fenster wechselt den Modus, sonst bleibt er wie er ist
    fn open(&mut self, source: &'static str, mode: Mode, explicit: bool) {
        if !self.window.is_visible() {
            self.source = source;
            self.window.set_mode(mode);
        } else if explicit && self.window.mode() != mode {
            self.window.set_mode(mode);
        }
        self.refresh_commits();
        self.window.show();
        self.start_scan();
    }

    pub fn handle_window_event(&mut self, id: WindowId, event: &WindowEvent) {
        if !matches!(event, WindowEvent::CloseRequested) {
            return;
        }
        if id == self.window.id() {
            self.window.hide();
        } else if id == self.today.id() {
            self.today.hide();
        } else if id == self.settings_win.id() {
            self.settings_win.hide();
        }
    }

    /// Menüpunkt "Heutige Einträge"
    pub fn open_today(&mut self) {
        self.today_date = Local::now().date_naive();
        self.refresh_today();
        self.today.show();
    }

    fn refresh_today(&self) {
        let view = today::view(self.today_date, Local::now().date_naive(), &self.journal.day(self.today_date));
        self.today.script(&format!("onDay({view})"));
    }

    fn refresh_today_if_visible(&self) {
        if self.today.is_visible() {
            self.refresh_today();
        }
    }

    pub fn handle_today(&mut self, msg: TodayMsg) {
        let now = Local::now().date_naive();
        let result = match msg {
            TodayMsg::Close => {
                self.today.hide();
                return;
            }
            TodayMsg::Nav { delta } => {
                self.today_date = today::shift(self.today_date, delta, now);
                Ok(())
            }
            TodayMsg::Today => {
                self.today_date = now;
                Ok(())
            }
            TodayMsg::UpdateNote { id, text } => self.journal.update(&id, Update::Note { text }).map(|_| ()),
            TodayMsg::UpdateReflection { id, review, reflection, mood } => self
                .journal
                .update(&id, Update::Reflection { review, reflection, mood })
                .map(|_| ()),
            TodayMsg::Delete { id } => self.delete_entry(&id),
        };
        match result {
            Ok(()) => self.refresh_today(),
            Err(e) => {
                log::warn(&format!("Heutige Einträge: {e}"));
                self.today.script(&format!("onError({})", js_string(&e)));
            }
        }
    }

    /// Löscht eine Notiz oder Reflexion. Commits und Tagesmarkierungen bleiben, sonst importiert Git sie neu.
    fn delete_entry(&mut self, id: &str) -> Result<(), String> {
        let entries = self.journal.day(self.today_date);
        match entries.iter().find(|e| e.id() == id) {
            Some(Entry::Note { .. } | Entry::Reflection { .. }) => self.journal.remove(id),
            Some(_) => Err("Commits und Tagesmarkierungen lassen sich hier nicht löschen".to_string()),
            None => Err("Eintrag nicht gefunden".to_string()),
        }
    }

    /// Menüpunkt "Journal-Ordner öffnen"
    pub fn open_journal_folder(&self) {
        let dir = self.journal.dir();
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::warn(&format!("Journal-Ordner nicht erstellbar: {e}"));
        }
        system::open_folder(dir);
    }

    /// Menüpunkt "Einstellungen ..."
    pub fn open_settings(&mut self) {
        let data = serde_json::json!({ "settings": self.settings, "autostart": system::autostart_enabled() });
        self.settings_win.script(&format!("onSettings({data})"));
        self.settings_win.show();
    }

    pub fn handle_settings(&mut self, msg: SettingsMsg) {
        match msg {
            SettingsMsg::Close => self.settings_win.hide(),
            SettingsMsg::Save(input) => match self.save_settings(*input) {
                Ok(()) => self.settings_win.hide(),
                Err(e) => {
                    log::warn(&format!("Einstellungen nicht gespeichert: {e}"));
                    self.settings_win.script(&format!("onError({})", js_string(&e)));
                }
            },
        }
    }

    fn save_settings(&mut self, input: SettingsInput) -> Result<(), String> {
        let (settings, autostart) = input.into_settings()?;
        let schedule = settings.schedule()?;
        let git = settings.git()?;
        check_git_folders(&git)?;
        settings.save()?;

        self.timer = Timer::new(schedule.clone(), Local::now().naive_local());
        self.schedule = schedule;
        self.git = git.enabled().then_some(git);
        self.next_git = Instant::now();
        self.settings = settings;

        system::set_autostart(autostart).map_err(|e| {
            format!("Die Einstellungen sind gespeichert, aber der Autostart konnte nicht geändert werden: {e}")
        })
    }

    pub fn handle_entry(&mut self, msg: EntryMsg) {
        match msg {
            EntryMsg::Save(text) => self.save(Entry::note(Utc::now(), &text, self.source)),
            EntryMsg::Reflect { review, reflection, mood } => {
                self.save(Entry::reflection(Utc::now(), &review, &reflection, &mood));
            }
            EntryMsg::Mode(mode) => self.window.set_mode(mode),
            EntryMsg::Later => {
                self.window.hide();
                self.timer.snooze(Local::now().naive_local());
                self.prompt.quiet();
            }
            EntryMsg::Skip => {
                self.window.hide();
                self.window.reset();
                self.reflection_pending = false;
                self.timer.answered();
                self.prompt.quiet();
            }
            EntryMsg::Close => self.window.hide(),
        }
    }

    fn save(&mut self, entry: Entry) {
        match self.journal.add(entry) {
            Ok(_) => {
                self.window.hide();
                self.window.reset();
                self.reflection_pending = false;
                self.timer.answered();
                self.prompt.saved(Instant::now());
                self.refresh_today_if_visible();
            }
            Err(e) => {
                log::warn(&format!("Eintrag nicht gespeichert: {e}"));
                self.window.error(&e);
            }
        }
    }
}

fn register_hotkey(proxy: EventLoopProxy<UserEvent>) -> Option<GlobalHotKeyManager> {
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(e) => {
            log::warn(&format!("Tastenkürzel nicht verfügbar: {e}"));
            return None;
        }
    };
    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyN);
    if let Err(e) = manager.register(hotkey) {
        log::warn(&format!("Tastenkürzel Strg+Alt+N nicht registriert (belegt?): {e}"));
        return None;
    }

    let id = hotkey.id();
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.id == id && e.state == HotKeyState::Pressed {
            proxy.send_event(UserEvent::Hotkey).ok();
        }
    }));
    Some(manager)
}

#[cfg(test)]
mod tests {
    use super::*;

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
