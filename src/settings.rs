use crate::export::{ExportConfig, MAX_HOURS_PER_DAY};
use crate::git::GitConfig;
use crate::hotkey;
use crate::limits::{self, RingColors, Thresholds};
use crate::log;
use crate::store;
use crate::timer::Schedule;
use chrono::{Datelike, NaiveDate, NaiveTime, Weekday};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const MAX_BLOCKS: usize = 8;
const MAX_ORTE: usize = 12;
const MAX_NAME: usize = 100;
const MAX_AUTO_TEXT: usize = 2_000;

/// Art eines Orts. Sie entscheidet über Check-ins (nur `Arbeit`) und die automatischen Texte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DayKind {
    Arbeit,
    Schule,
    Uek,
    Ferien,
}

/// Ein Ort wie `Noser Young` (ohne das `@`, das setzt der Export davor) mit seinem Tagessoll in Stunden
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ort {
    pub name: String,
    pub tagessoll: f64,
    pub art: DayKind,
}

impl Ort {
    fn new(name: &str, art: DayKind) -> Self {
        Self { name: name.into(), tagessoll: 8.4, art }
    }

    /// Tagessoll in Zehntelstunden
    pub fn soll_tenths(&self) -> i64 {
        (self.tagessoll.clamp(0.0, MAX_HOURS_PER_DAY) * 10.0).round() as i64
    }
}

/// Ort eines Wochentags im Wochenplan
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanTag {
    pub tag: Weekday,
    pub ort: String,
}

/// Feste Texte für eine reine Ferien- oder üK-Woche
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoTexte {
    pub taetigkeit: String,
    pub rueckblick: String,
    pub reflexion: String,
    pub stimmung: String,
}

impl AutoTexte {
    pub fn ferien() -> Self {
        Self {
            taetigkeit: "Ich habe die Ferien genossen".into(),
            rueckblick: "Es gibt keinen Wochenrückblick, da ich in den Ferien war.".into(),
            reflexion: "Es gibt keine Reflexion, da ich in den Ferien war.".into(),
            stimmung: "Es gibt keine Stimmung der Woche, da ich in den Ferien war.".into(),
        }
    }

    pub fn uek() -> Self {
        Self {
            taetigkeit: "Wir hatten üK".into(),
            rueckblick: "Diese Woche gibt es keinen Wochenrückblick, da wir einen üK hatten.".into(),
            reflexion: "Diese Woche gibt es keine Wochenreflexion, da wir einen üK hatten.".into(),
            stimmung: "Diese Woche gibt es keine Stimmung der Woche, da wir einen üK hatten.".into(),
        }
    }

    fn check(&self, label: &str) -> Result<(), String> {
        for (name, text) in [
            ("Tätigkeit", &self.taetigkeit),
            ("Wochenrückblick", &self.rueckblick),
            ("Reflexion", &self.reflexion),
            ("Stimmung", &self.stimmung),
        ] {
            if text.chars().count() > MAX_AUTO_TEXT {
                return Err(format!("{label}: {name} ist zu lang (höchstens {MAX_AUTO_TEXT} Zeichen)"));
            }
        }
        Ok(())
    }
}

impl Default for AutoTexte {
    fn default() -> Self {
        Self::ferien()
    }
}

/// Wohin das Panel neben der Pille aufklappt
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Aufklappen {
    /// Dorthin, wo mehr Platz bis zum Bildschirmrand ist
    #[default]
    Auto,
    Unten,
    Oben,
}

impl Aufklappen {
    /// Wert aus dem Einstellungsfenster (`auto`, `unten`, `oben`); alles andere ist ein Fehler
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim().to_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "unten" => Ok(Self::Unten),
            "oben" => Ok(Self::Oben),
            other => Err(format!("Die Aufklapprichtung \"{other}\" ist unbekannt (erlaubt: auto, unten, oben)")),
        }
    }
}

pub const MAX_SNOOZE_MINUTES: u32 = 240;

/// Name ohne Leerraum und ohne führendes `@`
pub fn clean_ort_name(name: &str) -> &str {
    name.trim().trim_start_matches('@').trim()
}

/// Wochenplan und Orte. Löst den Ort eines Tages auf: Override der Woche, sonst Wochenplan.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub wochenplan: Vec<PlanTag>,
    pub orte: Vec<Ort>,
}

impl Plan {
    pub fn find(&self, name: &str) -> Option<&Ort> {
        let name = clean_ort_name(name);
        self.orte.iter().find(|o| clean_ort_name(&o.name).eq_ignore_ascii_case(name))
    }

    /// Ort laut Wochenplan (`None` an Tagen ohne Eintrag, z. B. am Wochenende)
    pub fn planned(&self, date: NaiveDate) -> Option<&Ort> {
        let weekday = date.weekday();
        let entry = self.wochenplan.iter().find(|p| p.tag == weekday)?;
        self.find(&entry.ort)
    }

    /// Override der Woche (falls es den Ort gibt), sonst der Wochenplan
    pub fn resolve(&self, date: NaiveDate, override_name: Option<&str>) -> Option<&Ort> {
        override_name.and_then(|n| self.find(n)).or_else(|| self.planned(date))
    }

    /// Welcher Override entsteht, wenn der Tag auf `ort` gestellt wird? `None`, wenn es der Ort des
    /// Wochenplans ist (oder `ort` leer ist) und ein bestehender Override wegfällt. Ein unbekannter Ort ist ein Fehler.
    pub fn override_for(&self, date: NaiveDate, ort: &str) -> Result<Option<String>, String> {
        if clean_ort_name(ort).is_empty() {
            return Ok(None);
        }
        let found = self.find(ort).ok_or_else(|| format!("Der Ort \"{}\" ist unbekannt", clean_ort_name(ort)))?;
        let planned = self.planned(date).is_some_and(|p| p.name == found.name);
        Ok((!planned).then(|| found.name.clone()))
    }

    /// Erster Ort einer Art, z. B. für das Tray-Menü "Heute ist ..."
    pub fn first_of(&self, kind: DayKind) -> Option<&Ort> {
        self.orte.iter().find(|o| o.art == kind)
    }

    fn check(&self) -> Result<(), String> {
        if self.orte.is_empty() {
            return Err("Mindestens ein Ort ist nötig".to_string());
        }
        if self.orte.len() > MAX_ORTE {
            return Err(format!("Höchstens {MAX_ORTE} Orte sind erlaubt"));
        }
        for (i, ort) in self.orte.iter().enumerate() {
            let name = clean_ort_name(&ort.name);
            if name.is_empty() {
                return Err("Ein Ort hat keinen Namen".to_string());
            }
            if name.chars().count() > MAX_NAME {
                return Err(format!("Der Ort \"{name}\" ist zu lang (höchstens {MAX_NAME} Zeichen)"));
            }
            if !ort.tagessoll.is_finite() || !(0.0..=MAX_HOURS_PER_DAY).contains(&ort.tagessoll) {
                return Err(format!(
                    "Das Tagessoll von \"{name}\" muss zwischen 0 und {MAX_HOURS_PER_DAY} Stunden liegen"
                ));
            }
            if self.orte[..i].iter().any(|o| clean_ort_name(&o.name).eq_ignore_ascii_case(name)) {
                return Err(format!("Der Ort \"{name}\" kommt doppelt vor"));
            }
        }
        for (i, tag) in self.wochenplan.iter().enumerate() {
            if self.find(&tag.ort).is_none() {
                return Err(format!("Der Wochenplan nennt den unbekannten Ort \"{}\"", tag.ort));
            }
            if self.wochenplan[..i].iter().any(|p| p.tag == tag.tag) {
                return Err("Ein Wochentag steht doppelt im Wochenplan".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeBlock {
    pub start: String,
    pub end: String,
}

/// Einstellungen in `%APPDATA%\notify\settings.json`. Fehlende Felder bekommen den Standardwert,
/// unbekannte (z. B. `work_days`, `location` aus der alten Version) werden ignoriert.
/// Wochentage heissen `Mon`, `Tue`, `Wed`, `Thu`, `Fri`, `Sat`, `Sun`, Zeiten `HH:MM`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub work_blocks: Vec<TimeBlock>,
    pub interval_minutes: u32,
    pub away_minutes: u32,
    pub reflection_day: Weekday,
    pub git_folders: Vec<String>,
    pub git_emails: Vec<String>,
    pub export_dir: String,
    /// Standardmässig Mo-Mi `Noser Young`, Do-Fr `Gibb`
    pub wochenplan: Vec<PlanTag>,
    pub orte: Vec<Ort>,
    pub nachname: String,
    pub vorname: String,
    /// Ordner der Wochen-Dateien. Leer: `%APPDATA%\notify\weeks`.
    pub journal_dir: String,
    /// Eigene Word-Vorlage. Leer: die mitgelieferte Vorlage.
    pub vorlage_pfad: String,
    /// Fehlende Stunden bis zum Tagessoll beim Export auf die letzte Zeile des Tages schreiben
    pub rest_auffuellen: bool,
    pub ferien_texte: AutoTexte,
    pub uek_texte: AutoTexte,
    /// Globales Tastenkürzel für die Leiste, z. B. `Ctrl+Alt+J`
    pub hotkey: String,
    /// Hinweis, wenn ein neuer Git-Commit gefunden wurde
    pub commit_erinnerung: bool,
    /// Wohin das Panel aufklappt
    pub aufklappen: Aufklappen,
    /// Nach "In ... min" im Hinweis erinnert die App nach so vielen Minuten nochmal
    pub snooze_minutes: u32,
    /// Claude-Limits als Ringe in der Pille zeigen (aus: immer das Ort-Icon, die Datei wird nicht gelesen)
    pub claude_limits: bool,
    /// Ringfarben der Claude-Limits, "#rrggbb": aussen 5 Stunden, Mitte Woche, innen Kontext, Warnstufe ab 80 %, kritisch ab 95 %
    pub ring_five_hour: String,
    pub ring_seven_day: String,
    pub ring_context: String,
    pub ring_warn: String,
    pub ring_crit: String,
    /// Ab wie viel Prozent ein Ring die Warnfarbe bzw. die kritische Farbe bekommt (1 <= warn < crit <= 100)
    pub ring_warn_at: u32,
    pub ring_crit_at: u32,
}

fn plan_tag(tag: Weekday, ort: &str) -> PlanTag {
    PlanTag { tag, ort: ort.into() }
}

impl Default for Settings {
    fn default() -> Self {
        let block = |start: &str, end: &str| TimeBlock { start: start.into(), end: end.into() };
        Self {
            work_blocks: vec![block("08:00", "12:00"), block("13:00", "17:00")],
            interval_minutes: 60,
            away_minutes: 10,
            reflection_day: Weekday::Fri,
            git_folders: Vec::new(),
            git_emails: Vec::new(),
            export_dir: String::new(),
            wochenplan: vec![
                plan_tag(Weekday::Mon, "Noser Young"),
                plan_tag(Weekday::Tue, "Noser Young"),
                plan_tag(Weekday::Wed, "Noser Young"),
                plan_tag(Weekday::Thu, "Gibb"),
                plan_tag(Weekday::Fri, "Gibb"),
            ],
            orte: vec![
                Ort::new("Noser Young", DayKind::Arbeit),
                Ort::new("Gibb", DayKind::Schule),
                Ort::new("üK", DayKind::Uek),
                Ort::new("Ferien", DayKind::Ferien),
            ],
            nachname: String::new(),
            vorname: String::new(),
            journal_dir: String::new(),
            vorlage_pfad: String::new(),
            rest_auffuellen: false,
            ferien_texte: AutoTexte::ferien(),
            uek_texte: AutoTexte::uek(),
            hotkey: hotkey::DEFAULT.into(),
            commit_erinnerung: true,
            aufklappen: Aufklappen::Auto,
            snooze_minutes: 15,
            claude_limits: true,
            ring_five_hour: limits::DEFAULT_FIVE_HOUR.into(),
            ring_seven_day: limits::DEFAULT_SEVEN_DAY.into(),
            ring_context: limits::DEFAULT_CONTEXT.into(),
            ring_warn: limits::DEFAULT_WARN.into(),
            ring_crit: limits::DEFAULT_CRIT.into(),
            ring_warn_at: limits::DEFAULT_WARN_AT,
            ring_crit_at: limits::DEFAULT_CRIT_AT,
        }
    }
}

fn parse_time(label: &str, text: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(text.trim(), "%H:%M")
        .map_err(|_| format!("{label} \"{text}\" ist keine gültige Zeit (erwartet HH:MM, z. B. 08:00)"))
}

impl Settings {
    pub fn path() -> PathBuf {
        store::data_dir().join("settings.json")
    }

    /// Lädt die Datei. Fehlt sie, wird sie mit den Standardwerten angelegt.
    /// Ist sie kaputt, läuft die App mit den Standardwerten weiter und die Datei bleibt unangetastet.
    pub fn load() -> Self {
        let path = Self::path();
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(settings) => settings,
                Err(e) => {
                    log::error(&format!("settings.json unlesbar, Standardwerte aktiv: {e}"));
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let settings = Self::default();
                if let Err(e) = settings.save() {
                    log::warn(&e);
                }
                settings
            }
            Err(e) => {
                log::error(&format!("settings.json nicht lesbar, Standardwerte aktiv: {e}"));
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(Self::path(), json).map_err(|e| format!("Einstellungen nicht speicherbar: {e}"))
    }

    /// Wochenplan und Orte, um den Ort eines Tages aufzulösen
    pub fn plan(&self) -> Plan {
        Plan { wochenplan: self.wochenplan.clone(), orte: self.orte.clone() }
    }

    /// Ordner der Wochen-Dateien (Standard `%APPDATA%\notify\weeks`)
    pub fn journal_path(&self) -> PathBuf {
        let dir = self.journal_dir.trim();
        if dir.is_empty() { store::data_dir().join("weeks") } else { PathBuf::from(dir) }
    }

    /// Namen, Zielordner, Vorlage, Orte und feste Texte des Wochenexports
    pub fn export(&self) -> Result<ExportConfig, String> {
        let plan = self.plan();
        plan.check()?;
        for (label, name) in [("Nachname", &self.nachname), ("Vorname", &self.vorname)] {
            if name.chars().count() > MAX_NAME {
                return Err(format!("{label} ist zu lang (höchstens {MAX_NAME} Zeichen)"));
            }
        }
        self.ferien_texte.check("Ferien")?;
        self.uek_texte.check("üK")?;

        let dir = self.export_dir.trim();
        let vorlage = self.vorlage_pfad.trim();
        Ok(ExportConfig {
            nachname: self.nachname.trim().to_string(),
            vorname: self.vorname.trim().to_string(),
            dir: (!dir.is_empty()).then(|| PathBuf::from(dir)),
            vorlage: (!vorlage.is_empty()).then(|| PathBuf::from(vorlage)),
            rest_auffuellen: self.rest_auffuellen,
            plan,
            ferien: self.ferien_texte.clone(),
            uek: self.uek_texte.clone(),
        })
    }

    /// Die Ringfarben für Pille und Karte (eine unlesbare Farbe aus einer von Hand bearbeiteten Datei wird zum Standard)
    pub fn ring_colors(&self) -> RingColors {
        RingColors::resolve(&self.ring_five_hour, &self.ring_seven_day, &self.ring_context, &self.ring_warn, &self.ring_crit)
    }

    /// Die Schwellen für Warn- und kritische Farbe (ungültige Werte aus einer von Hand bearbeiteten Datei werden zum Standard)
    pub fn ring_thresholds(&self) -> Thresholds {
        Thresholds::resolve(self.ring_warn_at, self.ring_crit_at)
    }

    /// Das Tastenkürzel, geprüft
    pub fn hotkey(&self) -> Result<global_hotkey::hotkey::HotKey, String> {
        hotkey::parse_hotkey(&self.hotkey)
    }

    /// Git-Ordner und E-Mail-Adressen. Ohne Ordner oder E-Mail bleibt Git aus.
    pub fn git(&self) -> Result<GitConfig, String> {
        let mut folders = Vec::new();
        for f in &self.git_folders {
            let f = f.trim();
            if f.is_empty() {
                return Err("Ein Git-Ordner ist leer".to_string());
            }
            folders.push(PathBuf::from(f));
        }
        let mut emails = Vec::new();
        for e in &self.git_emails {
            let e = e.trim();
            let valid = e.contains('@')
                && !e.starts_with('@')
                && !e.ends_with('@')
                && !e.contains(char::is_whitespace);
            if !valid {
                return Err(format!("\"{e}\" ist keine gültige E-Mail-Adresse"));
            }
            emails.push(e.to_lowercase());
        }
        Ok(GitConfig { folders, emails })
    }

    /// Prüft alle Werte und gibt verständliche Fehler zurück, statt etwas still zu korrigieren
    pub fn schedule(&self) -> Result<Schedule, String> {
        if !(1..=480).contains(&self.interval_minutes) {
            return Err("Intervall muss zwischen 1 und 480 Minuten liegen".to_string());
        }
        if !(1..=240).contains(&self.away_minutes) {
            return Err("Weg-Schwelle muss zwischen 1 und 240 Minuten liegen".to_string());
        }
        if !(1..=MAX_SNOOZE_MINUTES).contains(&self.snooze_minutes) {
            return Err(format!("\"Später erinnern\" muss zwischen 1 und {MAX_SNOOZE_MINUTES} Minuten liegen"));
        }
        let plan = self.plan();
        plan.check()?;
        let has_work_day = plan.wochenplan.iter().any(|p| plan.find(&p.ort).is_some_and(|o| o.art == DayKind::Arbeit));
        if !has_work_day {
            return Err("Mindestens ein Arbeitstag ist nötig (ein Tag im Wochenplan mit einem Arbeits-Ort)".to_string());
        }
        if self.work_blocks.is_empty() {
            return Err("Mindestens eine Arbeitszeit ist nötig".to_string());
        }
        if self.work_blocks.len() > MAX_BLOCKS {
            return Err(format!("Höchstens {MAX_BLOCKS} Arbeitszeiten sind erlaubt"));
        }

        let mut blocks = Vec::new();
        for b in &self.work_blocks {
            let start = parse_time("Beginn", &b.start)?;
            let end = parse_time("Ende", &b.end)?;
            if end <= start {
                return Err(format!("Arbeitszeit {}–{}: Das Ende muss nach dem Beginn liegen", b.start, b.end));
            }
            blocks.push((start, end));
        }
        blocks.sort();
        for pair in blocks.windows(2) {
            if pair[1].0 < pair[0].1 {
                return Err(format!(
                    "Arbeitszeiten überschneiden sich: {}–{} und {}–{}",
                    pair[0].0.format("%H:%M"),
                    pair[0].1.format("%H:%M"),
                    pair[1].0.format("%H:%M"),
                    pair[1].1.format("%H:%M"),
                ));
            }
        }

        Ok(Schedule {
            plan,
            blocks,
            interval_minutes: self.interval_minutes,
            away_minutes: self.away_minutes,
            snooze_minutes: self.snooze_minutes,
            reflection_day: self.reflection_day,
        })
    }
}

/// Eingaben aus dem Einstellungsfenster. Zahlen kommen als Text, damit Fehler verständlich gemeldet werden.
/// Die Felder der Version 2 sind optional: fehlen sie, bleibt der bisherige Wert.
#[derive(Debug, Clone, Deserialize)]
pub struct SettingsInput {
    pub work_blocks: Vec<TimeBlock>,
    pub interval_minutes: String,
    pub away_minutes: String,
    pub reflection_day: Weekday,
    pub git_folders: Vec<String>,
    pub git_emails: Vec<String>,
    pub export_dir: String,
    pub autostart: bool,
    #[serde(default)]
    pub wochenplan: Option<Vec<PlanTag>>,
    #[serde(default)]
    pub orte: Option<Vec<Ort>>,
    #[serde(default)]
    pub nachname: Option<String>,
    #[serde(default)]
    pub vorname: Option<String>,
    #[serde(default)]
    pub journal_dir: Option<String>,
    #[serde(default)]
    pub vorlage_pfad: Option<String>,
    #[serde(default)]
    pub rest_auffuellen: Option<bool>,
    #[serde(default)]
    pub ferien_texte: Option<AutoTexte>,
    #[serde(default)]
    pub uek_texte: Option<AutoTexte>,
    #[serde(default)]
    pub hotkey: Option<String>,
    #[serde(default)]
    pub commit_erinnerung: Option<bool>,
    /// `auto`, `unten` oder `oben`
    #[serde(default)]
    pub aufklappen: Option<String>,
    #[serde(default)]
    pub snooze_minutes: Option<String>,
    #[serde(default)]
    pub claude_limits: Option<bool>,
    /// Ringfarben als "#RRGGBB" (auch "#RGB" oder ohne "#")
    #[serde(default)]
    pub ring_five_hour: Option<String>,
    #[serde(default)]
    pub ring_seven_day: Option<String>,
    #[serde(default)]
    pub ring_context: Option<String>,
    #[serde(default)]
    pub ring_warn: Option<String>,
    #[serde(default)]
    pub ring_crit: Option<String>,
    /// Schwellen in Prozent als Text (wie die anderen Zahlenfelder)
    #[serde(default)]
    pub ring_warn_at: Option<String>,
    #[serde(default)]
    pub ring_crit_at: Option<String>,
}

fn parse_whole(label: &str, text: &str) -> Result<u32, String> {
    text.trim()
        .parse::<u32>()
        .map_err(|_| format!("{label} muss eine ganze Zahl sein (z. B. 60)"))
}

/// Neue Ringfarbe prüfen und normalisieren; fehlt sie, bleibt die bisherige
fn ring_color(label: &str, input: Option<String>, base: &str) -> Result<String, String> {
    match input {
        Some(text) => limits::check_color(label, &text),
        None => Ok(base.to_string()),
    }
}

impl SettingsInput {
    /// Prüft alle Eingaben und gibt die Einstellungen samt Autostart-Wunsch zurück.
    /// Fehler werden gemeldet, nichts wird still korrigiert. Fehlende Felder übernimmt `base`.
    pub fn into_settings(self, base: &Settings) -> Result<(Settings, bool), String> {
        let settings = Settings {
            work_blocks: self.work_blocks,
            interval_minutes: parse_whole("Das Intervall", &self.interval_minutes)?,
            away_minutes: parse_whole("Die Weg-Schwelle", &self.away_minutes)?,
            reflection_day: self.reflection_day,
            git_folders: self.git_folders,
            git_emails: self.git_emails,
            export_dir: self.export_dir,
            wochenplan: self.wochenplan.unwrap_or_else(|| base.wochenplan.clone()),
            orte: self.orte.unwrap_or_else(|| base.orte.clone()),
            nachname: self.nachname.unwrap_or_else(|| base.nachname.clone()),
            vorname: self.vorname.unwrap_or_else(|| base.vorname.clone()),
            journal_dir: self.journal_dir.unwrap_or_else(|| base.journal_dir.clone()),
            vorlage_pfad: self.vorlage_pfad.unwrap_or_else(|| base.vorlage_pfad.clone()),
            rest_auffuellen: self.rest_auffuellen.unwrap_or(base.rest_auffuellen),
            ferien_texte: self.ferien_texte.unwrap_or_else(|| base.ferien_texte.clone()),
            uek_texte: self.uek_texte.unwrap_or_else(|| base.uek_texte.clone()),
            hotkey: self.hotkey.map_or_else(|| base.hotkey.clone(), |h| h.trim().to_string()),
            commit_erinnerung: self.commit_erinnerung.unwrap_or(base.commit_erinnerung),
            aufklappen: match &self.aufklappen {
                Some(text) => Aufklappen::parse(text)?,
                None => base.aufklappen,
            },
            snooze_minutes: match &self.snooze_minutes {
                Some(text) => parse_whole("\"Später erinnern\"", text)?,
                None => base.snooze_minutes,
            },
            claude_limits: self.claude_limits.unwrap_or(base.claude_limits),
            ring_five_hour: ring_color("5 Stunden", self.ring_five_hour, &base.ring_five_hour)?,
            ring_seven_day: ring_color("Woche", self.ring_seven_day, &base.ring_seven_day)?,
            ring_context: ring_color("Kontext", self.ring_context, &base.ring_context)?,
            ring_warn: ring_color("Warnung", self.ring_warn, &base.ring_warn)?,
            ring_crit: ring_color("Kritisch", self.ring_crit, &base.ring_crit)?,
            ring_warn_at: match &self.ring_warn_at {
                Some(text) => parse_whole("Die Warnschwelle", text)?,
                None => base.ring_warn_at,
            },
            ring_crit_at: match &self.ring_crit_at {
                Some(text) => parse_whole("Die kritische Schwelle", text)?,
                None => base.ring_crit_at,
            },
        };
        Thresholds::check(settings.ring_warn_at, settings.ring_crit_at)?;
        settings.schedule()?;
        settings.git()?;
        settings.export()?;
        settings.hotkey()?;
        Ok((settings, self.autostart))
    }
}

/// Die Git-Ordner müssen existieren, damit ein Tippfehler nicht unbemerkt bleibt
pub fn check_git_folders(config: &GitConfig) -> Result<(), String> {
    match config.folders.iter().find(|f| !f.is_dir()) {
        Some(folder) => Err(format!("Der Git-Ordner {} existiert nicht", folder.display())),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_blocks(blocks: &[(&str, &str)]) -> Settings {
        Settings {
            work_blocks: blocks
                .iter()
                .map(|(s, e)| TimeBlock { start: s.to_string(), end: e.to_string() })
                .collect(),
            ..Settings::default()
        }
    }

    fn date(d: u32) -> NaiveDate {
        // Oktober 2026: 5 = Montag
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
    }

    #[test]
    fn defaults_are_valid() {
        let s = Settings::default().schedule().unwrap();
        assert_eq!(s.blocks.len(), 2);
        assert_eq!(s.interval_minutes, 60);
        assert_eq!(s.plan.wochenplan.len(), 5);
        assert_eq!(s.reflection_day, Weekday::Fri);
        Settings::default().export().unwrap();
    }

    #[test]
    fn json_roundtrip_and_missing_fields() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), Settings::default());

        let partial: Settings = serde_json::from_str(r#"{"interval_minutes": 30}"#).unwrap();
        assert_eq!(partial.interval_minutes, 30);
        assert_eq!(partial.work_blocks, Settings::default().work_blocks);
        assert!(serde_json::from_str::<Settings>(r#"{"wochenplan": []}"#).is_ok());
        assert!(serde_json::from_str::<Settings>("kein json").is_err());
    }

    #[test]
    fn claude_limits_switch_defaults_on_and_is_kept() {
        assert!(Settings::default().claude_limits);
        // alte Datei ohne das Feld: an
        let old: Settings = serde_json::from_str(r#"{"interval_minutes":45}"#).unwrap();
        assert!(old.claude_limits);
        let off: Settings = serde_json::from_str(r#"{"claude_limits":false}"#).unwrap();
        assert!(!off.claude_limits);
        // die Eingabe ändert den Wert, fehlt sie, bleibt er
        let base = Settings { claude_limits: false, ..Settings::default() };
        let (s, _) = input().into_settings(&base).unwrap();
        assert!(!s.claude_limits);
        let mut i = input();
        i.claude_limits = Some(true);
        assert!(i.into_settings(&base).unwrap().0.claude_limits);
        // Roundtrip über die Datei-Darstellung
        let text = serde_json::to_string(&Settings { claude_limits: false, ..Settings::default() }).unwrap();
        assert!(!serde_json::from_str::<Settings>(&text).unwrap().claude_limits);
    }

    #[test]
    fn ring_colors_have_defaults_and_old_files_still_load() {
        let d = Settings::default();
        assert_eq!(
            (d.ring_five_hour.as_str(), d.ring_seven_day.as_str(), d.ring_context.as_str()),
            ("#ededed", "#8a8a8a", "#5c5c5c")
        );
        assert_eq!((d.ring_warn.as_str(), d.ring_crit.as_str()), ("#d6b878", "#d28f8f"));
        assert_eq!(d.ring_colors(), RingColors::default());
        // alte Datei ohne die Felder (auch mit claude_limits, aber ohne Farben)
        for old in [r#"{"interval_minutes":45}"#, r#"{"claude_limits":false,"hotkey":"Ctrl+Alt+K"}"#] {
            let s: Settings = serde_json::from_str(old).unwrap();
            assert_eq!(s.ring_colors(), RingColors::default(), "{old}");
        }
        // Roundtrip mit eigenen Farben
        let custom = Settings { ring_five_hour: "#8aa8cc".into(), ring_crit: "#ff0000".into(), ..Settings::default() };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&custom).unwrap()).unwrap();
        assert_eq!(back, custom);
        // von Hand kaputt gemacht: die Fenster bekommen trotzdem gültige Farben
        let broken: Settings = serde_json::from_str(r#"{"ring_context":"blau","ring_warn":"D6B"}"#).unwrap();
        let c = broken.ring_colors();
        assert_eq!((c.context.as_str(), c.warn.as_str()), ("#5c5c5c", "#dd66bb"));
    }

    #[test]
    fn ring_thresholds_default_validate_and_survive_hand_edits() {
        let d = Settings::default();
        assert_eq!((d.ring_warn_at, d.ring_crit_at), (80, 95));
        assert_eq!(d.ring_thresholds(), Thresholds::default());
        // alte Datei ohne die Felder
        let old: Settings = serde_json::from_str(r#"{"interval_minutes":45}"#).unwrap();
        assert_eq!(old.ring_thresholds(), Thresholds::default());
        // von Hand kaputt gemacht: Standard statt Absturz
        let broken: Settings = serde_json::from_str(r#"{"ring_warn_at":99,"ring_crit_at":50}"#).unwrap();
        assert_eq!(broken.ring_thresholds(), Thresholds::default());
        // Eingabe: gültig, fehlend, ungültig
        let base = Settings { ring_warn_at: 70, ring_crit_at: 90, ..Settings::default() };
        let (s, _) = input().into_settings(&base).unwrap();
        assert_eq!((s.ring_warn_at, s.ring_crit_at), (70, 90));
        let mut i = input();
        i.ring_warn_at = Some(" 60 ".into());
        i.ring_crit_at = Some("100".into());
        let (s, _) = i.into_settings(&base).unwrap();
        assert_eq!((s.ring_warn_at, s.ring_crit_at), (60, 100));
        let err = |f: &dyn Fn(&mut SettingsInput)| {
            let mut i = input();
            f(&mut i);
            i.into_settings(&base).unwrap_err()
        };
        assert!(err(&|i| i.ring_warn_at = Some("viel".into())).contains("Warnschwelle"));
        assert!(err(&|i| i.ring_crit_at = Some("".into())).contains("kritische Schwelle"));
        assert!(err(&|i| i.ring_warn_at = Some("0".into())).contains("1 und 100"));
        assert!(err(&|i| i.ring_crit_at = Some("101".into())).contains("1 und 100"));
        assert!(err(&|i| { i.ring_warn_at = Some("90".into()); i.ring_crit_at = Some("90".into()); }).contains("unter"));
        assert!(err(&|i| i.ring_warn_at = Some("95".into())).contains("unter"), "gegen den bisherigen Wert 90");
    }

    #[test]
    fn ring_colors_from_input_are_validated_and_normalized() {
        let base = Settings { ring_context: "#123456".into(), ..Settings::default() };
        // nicht gelieferte Farben bleiben
        let (s, _) = input().into_settings(&base).unwrap();
        assert_eq!(s.ring_context, "#123456");
        assert_eq!(s.ring_five_hour, "#ededed");
        // #RGB, Gross-/Kleinschreibung, ohne # und Leerraum werden zu #rrggbb
        let mut i = input();
        i.ring_five_hour = Some("#ABC".into());
        i.ring_seven_day = Some(" 8AA8CC ".into());
        i.ring_context = Some("#A99BC7".into());
        i.ring_warn = Some("fa0".into());
        i.ring_crit = Some("#D28F8F".into());
        let (s, _) = i.into_settings(&base).unwrap();
        assert_eq!(
            [&s.ring_five_hour, &s.ring_seven_day, &s.ring_context, &s.ring_warn, &s.ring_crit],
            ["#aabbcc", "#8aa8cc", "#a99bc7", "#ffaa00", "#d28f8f"]
        );
        // Müll ist ein Fehler, der die Farbe nennt, und nichts wird gespeichert
        let err = |f: &dyn Fn(&mut SettingsInput)| {
            let mut i = input();
            f(&mut i);
            i.into_settings(&base).unwrap_err()
        };
        assert!(err(&|i| i.ring_five_hour = Some("rot".into())).contains("5 Stunden"));
        assert!(err(&|i| i.ring_seven_day = Some("#12345".into())).contains("Woche"));
        assert!(err(&|i| i.ring_context = Some("".into())).contains("Kontext"));
        assert!(err(&|i| i.ring_warn = Some("#gggggg".into())).contains("Warnung"));
        assert!(err(&|i| i.ring_crit = Some("#fffffff".into())).contains("Kritisch"));
        // eine dunkle Farbe wird trotzdem gespeichert
        let mut i = input();
        i.ring_context = Some("#1c1c1c".into());
        assert_eq!(i.into_settings(&base).unwrap().0.ring_context, "#1c1c1c");
    }

    #[test]
    fn old_settings_file_is_still_readable() {
        // Version 1 kannte work_days, school_days, location und hours_per_day
        let old = r#"{"work_days":["Mon","Tue"],"work_blocks":[{"start":"07:30","end":"11:30"}],
            "interval_minutes":45,"away_minutes":10,"school_days":["Wed"],"reflection_day":"Thu",
            "git_folders":["C:\\Work"],"git_emails":["a@b.ch"],"location":"@Firma","hours_per_day":7.5,
            "export_dir":"C:\\Temp"}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.interval_minutes, 45);
        assert_eq!(s.reflection_day, Weekday::Thu);
        assert_eq!(s.git_folders, ["C:\\Work"]);
        assert_eq!(s.export_dir, "C:\\Temp");
        // neue Felder bekommen die Standardwerte
        let d = Settings::default();
        assert_eq!(s.wochenplan, d.wochenplan);
        assert_eq!(s.orte, d.orte);
        assert_eq!(s.ferien_texte, d.ferien_texte);
        assert!(!s.rest_auffuellen);
        assert!(s.vorlage_pfad.is_empty() && s.journal_dir.is_empty());
        assert_eq!((s.hotkey.as_str(), s.commit_erinnerung), ("Ctrl+Alt+J", true));
        s.schedule().unwrap();
    }

    #[test]
    fn default_plan_and_places() {
        let d = Settings::default();
        let plan = d.plan();
        let place = |day| plan.planned(date(day)).map(|o| o.name.as_str());
        assert_eq!(place(5), Some("Noser Young"));
        assert_eq!(place(7), Some("Noser Young"));
        assert_eq!(place(8), Some("Gibb"));
        assert_eq!(place(9), Some("Gibb"));
        assert_eq!(place(10), None);
        assert!(d.orte.iter().all(|o| o.tagessoll == 8.4));
        assert_eq!(d.orte[0].soll_tenths(), 84);
        assert_eq!(d.ferien_texte.taetigkeit, "Ich habe die Ferien genossen");
        assert_eq!(d.uek_texte.taetigkeit, "Wir hatten üK");
    }

    #[test]
    fn override_beats_plan_and_unknown_override_is_ignored() {
        let plan = Settings::default().plan();
        let name = |o: Option<&Ort>| o.map(|o| o.name.clone());
        assert_eq!(name(plan.resolve(date(5), None)), Some("Noser Young".into()));
        assert_eq!(name(plan.resolve(date(5), Some("Ferien"))), Some("Ferien".into()));
        // `@`, Gross-/Kleinschreibung und Leerraum spielen keine Rolle
        assert_eq!(name(plan.resolve(date(5), Some(" @üK "))), Some("üK".into()));
        assert_eq!(name(plan.resolve(date(5), Some("gibb"))), Some("Gibb".into()));
        // unbekannter Ort: es gilt der Wochenplan
        assert_eq!(name(plan.resolve(date(5), Some("Mond"))), Some("Noser Young".into()));
        // Samstag: ohne Override kein Ort, mit Override schon
        assert_eq!(name(plan.resolve(date(10), None)), None);
        assert_eq!(name(plan.resolve(date(10), Some("Noser Young"))), Some("Noser Young".into()));
        assert_eq!(plan.first_of(DayKind::Uek).map(|o| o.name.as_str()), Some("üK"));
    }

    #[test]
    fn override_for_drops_the_planned_place() {
        let plan = Settings::default().plan();
        // Montag ist laut Plan @Noser Young
        assert_eq!(plan.override_for(date(5), "Noser Young"), Ok(None));
        assert_eq!(plan.override_for(date(5), " @noser young "), Ok(None));
        assert_eq!(plan.override_for(date(5), "Ferien"), Ok(Some("Ferien".into())));
        assert_eq!(plan.override_for(date(5), "gibb"), Ok(Some("Gibb".into())));
        assert_eq!(plan.override_for(date(5), ""), Ok(None));
        assert!(plan.override_for(date(5), "Mond").unwrap_err().contains("Mond"));
        // Samstag hat keinen Plan: jeder Ort ist ein Override
        assert_eq!(plan.override_for(date(10), "Noser Young"), Ok(Some("Noser Young".into())));
    }

    #[test]
    fn plan_is_checked() {
        let mut s = Settings::default();
        s.wochenplan[0].ort = "Nirgends".into();
        assert!(s.schedule().unwrap_err().contains("unbekannten Ort"));
        s = Settings::default();
        s.orte.push(Ort::new("gibb", DayKind::Schule));
        assert!(s.export().unwrap_err().contains("doppelt"));
        s = Settings::default();
        s.orte[1].tagessoll = 25.0;
        assert!(s.export().unwrap_err().contains("Tagessoll"));
        s.orte[1].tagessoll = f64::NAN;
        assert!(s.export().is_err());
        s = Settings::default();
        s.orte[0].name = " @ ".into();
        assert!(s.export().is_err());
        s = Settings::default();
        s.orte.clear();
        assert!(s.export().is_err());
        s = Settings::default();
        s.wochenplan.push(plan_tag(Weekday::Mon, "Gibb"));
        assert!(s.export().unwrap_err().contains("doppelt"));
        // ohne Arbeits-Ort im Plan gibt es keine Check-ins
        s = Settings::default();
        s.wochenplan = vec![plan_tag(Weekday::Mon, "Gibb")];
        assert!(s.schedule().unwrap_err().contains("Arbeitstag"));
    }

    #[test]
    fn git_settings_are_checked() {
        let mut s = Settings::default();
        assert!(!s.git().unwrap().enabled());

        s.git_folders = vec![" C:\\Work ".into()];
        s.git_emails = vec!["Me@Example.com ".into()];
        let g = s.git().unwrap();
        assert!(g.enabled());
        assert_eq!(g.folders, vec![PathBuf::from("C:\\Work")]);
        assert_eq!(g.emails, ["me@example.com"]);

        for bad in ["", "ohne-at", "@x.ch", "a@", "a b@x.ch"] {
            s.git_emails = vec![bad.into()];
            assert!(s.git().is_err(), "{bad}");
        }
        s.git_emails = vec![];
        s.git_folders = vec!["  ".into()];
        assert!(s.git().is_err());
    }

    #[test]
    fn export_settings_are_checked() {
        let c = Settings::default().export().unwrap();
        assert_eq!((c.nachname.as_str(), c.vorname.as_str(), c.dir, c.vorlage, c.rest_auffuellen), ("", "", None, None, false));

        let s = Settings {
            export_dir: " C:\\Temp\\ABJ ".into(),
            vorlage_pfad: " C:\\Vorlagen\\abj.docx ".into(),
            nachname: " Maurer ".into(),
            vorname: "Jemuel".into(),
            rest_auffuellen: true,
            ..Settings::default()
        };
        let c = s.export().unwrap();
        assert_eq!(c.dir, Some(PathBuf::from("C:\\Temp\\ABJ")));
        assert_eq!(c.vorlage, Some(PathBuf::from("C:\\Vorlagen\\abj.docx")));
        assert_eq!((c.nachname.as_str(), c.vorname.as_str(), c.rest_auffuellen), ("Maurer", "Jemuel", true));

        let s = Settings { nachname: "x".repeat(101), ..Settings::default() };
        assert!(s.export().is_err());
        let mut s = Settings::default();
        s.uek_texte.stimmung = "x".repeat(MAX_AUTO_TEXT + 1);
        assert!(s.export().unwrap_err().contains("üK"));
    }

    #[test]
    fn journal_path_defaults_to_weeks_folder() {
        let p = Settings::default().journal_path();
        assert!(p.ends_with("weeks"), "{}", p.display());
        let s = Settings { journal_dir: " D:\\ABJ ".into(), ..Settings::default() };
        assert_eq!(s.journal_path(), PathBuf::from("D:\\ABJ"));
    }

    fn input() -> SettingsInput {
        let d = Settings::default();
        SettingsInput {
            work_blocks: d.work_blocks,
            interval_minutes: "60".into(),
            away_minutes: "10".into(),
            reflection_day: Weekday::Fri,
            git_folders: vec![],
            git_emails: vec![],
            export_dir: String::new(),
            autostart: true,
            wochenplan: None,
            orte: None,
            nachname: None,
            vorname: None,
            journal_dir: None,
            vorlage_pfad: None,
            rest_auffuellen: None,
            ferien_texte: None,
            uek_texte: None,
            hotkey: None,
            commit_erinnerung: None,
            aufklappen: None,
            snooze_minutes: None,
            claude_limits: None,
            ring_five_hour: None,
            ring_seven_day: None,
            ring_context: None,
            ring_warn: None,
            ring_crit: None,
            ring_warn_at: None,
            ring_crit_at: None,
        }
    }

    #[test]
    fn input_becomes_settings() {
        let base = Settings { nachname: "Maurer".into(), ..Settings::default() };
        let (s, autostart) = input().into_settings(&base).unwrap();
        assert!(autostart);
        assert_eq!((s.interval_minutes, s.away_minutes), (60, 10));
        // nicht gelieferte Felder bleiben
        assert_eq!(s.nachname, "Maurer");
        assert_eq!(s.wochenplan, base.wochenplan);

        let mut i = input();
        i.interval_minutes = " 30 ".into();
        i.vorname = Some("Jemuel".into());
        i.rest_auffuellen = Some(true);
        i.uek_texte = Some(AutoTexte { taetigkeit: "üK-Woche".into(), ..AutoTexte::uek() });
        i.wochenplan = Some(vec![plan_tag(Weekday::Mon, "Noser Young")]);
        let (s, _) = i.into_settings(&base).unwrap();
        assert_eq!((s.interval_minutes, s.vorname.as_str(), s.rest_auffuellen), (30, "Jemuel", true));
        assert_eq!(s.uek_texte.taetigkeit, "üK-Woche");
        assert_eq!(s.wochenplan.len(), 1);

        // Hotkey und Commit-Erinnerung
        let mut i = input();
        i.hotkey = Some(" Strg+Shift+K ".into());
        i.commit_erinnerung = Some(false);
        let (s, _) = i.into_settings(&base).unwrap();
        assert_eq!((s.hotkey.as_str(), s.commit_erinnerung), ("Strg+Shift+K", false));
        assert!(s.hotkey().is_ok());
        let (s, _) = input().into_settings(&base).unwrap();
        assert_eq!((s.hotkey, s.commit_erinnerung), (base.hotkey, base.commit_erinnerung));
    }

    #[test]
    fn input_errors_are_readable() {
        let base = Settings::default();
        let err = |f: &dyn Fn(&mut SettingsInput)| {
            let mut i = input();
            f(&mut i);
            i.into_settings(&base).unwrap_err()
        };
        assert!(err(&|i| i.interval_minutes = "oft".into()).contains("Intervall"));
        assert!(err(&|i| i.interval_minutes = "-5".into()).contains("ganze Zahl"));
        assert!(err(&|i| i.interval_minutes = "0".into()).contains("zwischen 1 und 480"));
        assert!(err(&|i| i.interval_minutes = "99999999999".into()).contains("ganze Zahl"));
        assert!(err(&|i| i.away_minutes = "".into()).contains("Weg-Schwelle"));
        assert!(err(&|i| i.work_blocks.clear()).contains("Arbeitszeit"));
        assert!(err(&|i| i.work_blocks[0].start = "".into()).contains("keine gültige Zeit"));
        assert!(err(&|i| i.work_blocks[1].start = "11:00".into()).contains("überschneiden"));
        assert!(err(&|i| i.git_emails = vec!["keine-mail".into()]).contains("E-Mail"));
        assert!(err(&|i| i.hotkey = Some("J".into())).contains("Tastenkürzel"));
        assert!(err(&|i| i.hotkey = Some("".into())).contains("Tastenkürzel"));
        assert!(err(&|i| i.orte = Some(vec![])).contains("Ort"));
        assert!(err(&|i| i.aufklappen = Some("seitlich".into())).contains("Aufklapprichtung"));
        assert!(err(&|i| i.aufklappen = Some("".into())).contains("Aufklapprichtung"));
        assert!(err(&|i| i.snooze_minutes = Some("bald".into())).contains("Später erinnern"));
        assert!(err(&|i| i.snooze_minutes = Some("0".into())).contains("zwischen 1 und 240"));
        assert!(err(&|i| i.snooze_minutes = Some("241".into())).contains("zwischen 1 und 240"));
        assert!(err(&|i| i.snooze_minutes = Some("-1".into())).contains("ganze Zahl"));
        assert!(err(&|i| i.wochenplan = Some(vec![plan_tag(Weekday::Mon, "Mond")])).contains("Mond"));
    }

    #[test]
    fn new_fields_have_defaults_and_validation() {
        let d = Settings::default();
        assert_eq!((d.aufklappen, d.snooze_minutes), (Aufklappen::Auto, 15));
        assert_eq!(d.schedule().unwrap().snooze_minutes, 15);
        // alte settings.json ohne die neuen Felder
        let old: Settings = serde_json::from_str(r#"{"interval_minutes": 30}"#).unwrap();
        assert_eq!((old.aufklappen, old.snooze_minutes), (Aufklappen::Auto, 15));
        // Serialisierung als Text
        assert!(serde_json::to_string(&d).unwrap().contains(r#""aufklappen":"auto""#));
        let s: Settings = serde_json::from_str(r#"{"aufklappen":"oben","snooze_minutes":5}"#).unwrap();
        assert_eq!((s.aufklappen, s.snooze_minutes), (Aufklappen::Oben, 5));
        assert!(serde_json::from_str::<Settings>(r#"{"aufklappen":"links"}"#).is_err());

        assert_eq!(Aufklappen::parse("auto"), Ok(Aufklappen::Auto));
        assert_eq!(Aufklappen::parse(" Unten "), Ok(Aufklappen::Unten));
        assert_eq!(Aufklappen::parse("OBEN"), Ok(Aufklappen::Oben));
        assert!(Aufklappen::parse("rechts").is_err());

        for bad in [0, 241, 100_000] {
            let s = Settings { snooze_minutes: bad, ..Settings::default() };
            assert!(s.schedule().unwrap_err().contains("Später erinnern"), "{bad}");
        }
        for ok in [1, 240] {
            assert!(Settings { snooze_minutes: ok, ..Settings::default() }.schedule().is_ok());
        }
    }

    #[test]
    fn input_carries_the_new_fields() {
        let base = Settings::default();
        let mut i = input();
        i.aufklappen = Some("oben".into());
        i.snooze_minutes = Some(" 20 ".into());
        let (s, _) = i.into_settings(&base).unwrap();
        assert_eq!((s.aufklappen, s.snooze_minutes), (Aufklappen::Oben, 20));
        // nicht gelieferte Felder bleiben
        let kept = Settings { aufklappen: Aufklappen::Unten, snooze_minutes: 7, ..Settings::default() };
        let (s, _) = input().into_settings(&kept).unwrap();
        assert_eq!((s.aufklappen, s.snooze_minutes), (Aufklappen::Unten, 7));
    }

    #[test]
    fn git_folders_must_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = GitConfig { folders: vec![tmp.path().to_path_buf()], emails: vec!["a@b.ch".into()] };
        assert!(check_git_folders(&ok).is_ok());
        let bad = GitConfig { folders: vec![tmp.path().to_path_buf(), tmp.path().join("fehlt")], emails: vec![] };
        let err = check_git_folders(&bad).unwrap_err();
        assert!(err.contains("fehlt") && err.contains("existiert nicht"), "{err}");
        assert!(check_git_folders(&GitConfig { folders: vec![], emails: vec![] }).is_ok());
    }

    #[test]
    fn rejects_bad_numbers() {
        for interval in [0, 481] {
            let s = Settings { interval_minutes: interval, ..Settings::default() };
            assert!(s.schedule().is_err());
        }
        for away in [0, 241] {
            let s = Settings { away_minutes: away, ..Settings::default() };
            assert!(s.schedule().is_err());
        }
    }

    #[test]
    fn rejects_bad_blocks() {
        assert!(with_blocks(&[]).schedule().is_err());
        assert!(with_blocks(&[("12:00", "08:00")]).schedule().is_err());
        assert!(with_blocks(&[("08:00", "08:00")]).schedule().is_err());
        assert!(with_blocks(&[("8 Uhr", "12:00")]).schedule().is_err());
        assert!(with_blocks(&[("08:00", "25:00")]).schedule().is_err());
        let err = with_blocks(&[("08:00", "12:00"), ("11:00", "15:00")]).schedule().unwrap_err();
        assert!(err.contains("überschneiden"), "{err}");
        // Reihenfolge egal, direkt aneinander ist erlaubt
        assert!(with_blocks(&[("13:00", "17:00"), ("08:00", "13:00")]).schedule().is_ok());
    }

    #[test]
    fn too_many_blocks_are_rejected() {
        let blocks: Vec<_> = (0..9).map(|i| (format!("{:02}:00", i * 2), format!("{:02}:30", i * 2))).collect();
        let refs: Vec<_> = blocks.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        assert!(with_blocks(&refs).schedule().is_err());
    }
}
