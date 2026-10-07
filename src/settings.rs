use crate::export::{ExportConfig, MAX_HOURS_PER_DAY};
use crate::git::GitConfig;
use crate::log;
use crate::store;
use crate::timer::Schedule;
use chrono::{NaiveTime, Weekday};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const MAX_BLOCKS: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeBlock {
    pub start: String,
    pub end: String,
}

/// Einstellungen in `%APPDATA%\notify\settings.json`. Fehlende Felder bekommen den Standardwert.
/// Wochentage heissen `Mon`, `Tue`, `Wed`, `Thu`, `Fri`, `Sat`, `Sun`, Zeiten `HH:MM`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub work_days: Vec<Weekday>,
    pub work_blocks: Vec<TimeBlock>,
    pub interval_minutes: u32,
    pub away_minutes: u32,
    pub school_days: Vec<Weekday>,
    pub reflection_day: Weekday,
    pub git_folders: Vec<String>,
    pub git_emails: Vec<String>,
    pub location: String,
    pub hours_per_day: f64,
    pub export_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        let block = |start: &str, end: &str| TimeBlock { start: start.into(), end: end.into() };
        Self {
            work_days: vec![Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri],
            work_blocks: vec![block("08:00", "12:00"), block("13:00", "17:00")],
            interval_minutes: 60,
            away_minutes: 10,
            school_days: Vec::new(),
            reflection_day: Weekday::Fri,
            git_folders: Vec::new(),
            git_emails: Vec::new(),
            location: "@Firma".into(),
            hours_per_day: 8.4,
            export_dir: String::new(),
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

    /// Ort, Arbeitszeit pro Tag und Zielordner des Wochenexports
    pub fn export(&self) -> Result<ExportConfig, String> {
        let location = self.location.trim();
        if location.is_empty() {
            return Err("Der Ort für den Export darf nicht leer sein".to_string());
        }
        if location.chars().count() > 100 {
            return Err("Der Ort für den Export ist zu lang (höchstens 100 Zeichen)".to_string());
        }
        let hours = self.hours_per_day;
        if !hours.is_finite() || !(0.0..=MAX_HOURS_PER_DAY).contains(&hours) {
            return Err(format!("Die Arbeitszeit pro Tag muss zwischen 0 und {MAX_HOURS_PER_DAY} Stunden liegen"));
        }
        let dir = self.export_dir.trim();
        Ok(ExportConfig {
            location: location.to_string(),
            hours_per_day: hours,
            dir: (!dir.is_empty()).then(|| PathBuf::from(dir)),
        })
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
        if self.work_days.is_empty() {
            return Err("Mindestens ein Arbeitstag ist nötig".to_string());
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
            work_days: self.work_days.clone(),
            blocks,
            interval_minutes: self.interval_minutes,
            away_minutes: self.away_minutes,
            school_days: self.school_days.clone(),
            reflection_day: self.reflection_day,
        })
    }
}

/// Eingaben aus dem Einstellungsfenster. Zahlen kommen als Text, damit Fehler verständlich gemeldet werden.
#[derive(Debug, Clone, Deserialize)]
pub struct SettingsInput {
    pub work_days: Vec<Weekday>,
    pub work_blocks: Vec<TimeBlock>,
    pub interval_minutes: String,
    pub away_minutes: String,
    pub school_days: Vec<Weekday>,
    pub reflection_day: Weekday,
    pub git_folders: Vec<String>,
    pub git_emails: Vec<String>,
    pub location: String,
    pub hours_per_day: String,
    pub export_dir: String,
    pub autostart: bool,
}

fn parse_whole(label: &str, text: &str) -> Result<u32, String> {
    text.trim()
        .parse::<u32>()
        .map_err(|_| format!("{label} muss eine ganze Zahl sein (z. B. 60)"))
}

impl SettingsInput {
    /// Prüft alle Eingaben und gibt die Einstellungen samt Autostart-Wunsch zurück.
    /// Fehler werden gemeldet, nichts wird still korrigiert.
    pub fn into_settings(self) -> Result<(Settings, bool), String> {
        let hours = self.hours_per_day.trim().replace(',', ".").parse::<f64>().map_err(|_| {
            "Die Arbeitszeit pro Tag muss eine Zahl sein (z. B. 8.4 oder 8,4)".to_string()
        })?;
        let settings = Settings {
            work_days: self.work_days,
            work_blocks: self.work_blocks,
            interval_minutes: parse_whole("Das Intervall", &self.interval_minutes)?,
            away_minutes: parse_whole("Die Weg-Schwelle", &self.away_minutes)?,
            school_days: self.school_days,
            reflection_day: self.reflection_day,
            git_folders: self.git_folders,
            git_emails: self.git_emails,
            location: self.location,
            hours_per_day: hours,
            export_dir: self.export_dir,
        };
        settings.schedule()?;
        settings.git()?;
        settings.export()?;
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

    #[test]
    fn defaults_are_valid() {
        let s = Settings::default().schedule().unwrap();
        assert_eq!(s.blocks.len(), 2);
        assert_eq!(s.interval_minutes, 60);
        assert_eq!(s.work_days.len(), 5);
        assert_eq!(s.reflection_day, Weekday::Fri);
    }

    #[test]
    fn json_roundtrip_and_missing_fields() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), Settings::default());

        let partial: Settings = serde_json::from_str(r#"{"interval_minutes": 30}"#).unwrap();
        assert_eq!(partial.interval_minutes, 30);
        assert_eq!(partial.work_blocks, Settings::default().work_blocks);
        assert!(serde_json::from_str::<Settings>(r#"{"work_days": ["Mon", "Tue"]}"#).is_ok());
        assert!(serde_json::from_str::<Settings>("kein json").is_err());
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
        assert_eq!((c.location.as_str(), c.hours_per_day, c.dir), ("@Firma", 8.4, None));

        let s = Settings { export_dir: " C:\\Temp\\ABJ ".into(), hours_per_day: 0.0, ..Settings::default() };
        let c = s.export().unwrap();
        assert_eq!(c.dir, Some(PathBuf::from("C:\\Temp\\ABJ")));
        assert_eq!(c.hours_per_day, 0.0);

        for hours in [-0.1, 24.1, f64::NAN, f64::INFINITY] {
            let s = Settings { hours_per_day: hours, ..Settings::default() };
            assert!(s.export().is_err(), "{hours}");
        }
        let s = Settings { location: "  ".into(), ..Settings::default() };
        assert!(s.export().is_err());
        let s = Settings { location: "x".repeat(101), ..Settings::default() };
        assert!(s.export().is_err());
    }

    fn input() -> SettingsInput {
        let d = Settings::default();
        SettingsInput {
            work_days: d.work_days,
            work_blocks: d.work_blocks,
            interval_minutes: "60".into(),
            away_minutes: "10".into(),
            school_days: vec![Weekday::Wed],
            reflection_day: Weekday::Fri,
            git_folders: vec![],
            git_emails: vec![],
            location: "@Firma".into(),
            hours_per_day: "8,4".into(),
            export_dir: String::new(),
            autostart: true,
        }
    }

    #[test]
    fn input_becomes_settings() {
        let (s, autostart) = input().into_settings().unwrap();
        assert!(autostart);
        assert_eq!((s.interval_minutes, s.away_minutes, s.hours_per_day), (60, 10, 8.4));
        assert_eq!(s.school_days, vec![Weekday::Wed]);

        let mut i = input();
        i.hours_per_day = " 7.5 ".into();
        i.interval_minutes = " 30 ".into();
        let (s, _) = i.into_settings().unwrap();
        assert_eq!((s.interval_minutes, s.hours_per_day), (30, 7.5));
    }

    #[test]
    fn input_errors_are_readable() {
        let err = |f: &dyn Fn(&mut SettingsInput)| {
            let mut i = input();
            f(&mut i);
            i.into_settings().unwrap_err()
        };
        assert!(err(&|i| i.interval_minutes = "oft".into()).contains("Intervall"));
        assert!(err(&|i| i.interval_minutes = "-5".into()).contains("ganze Zahl"));
        assert!(err(&|i| i.interval_minutes = "0".into()).contains("zwischen 1 und 480"));
        assert!(err(&|i| i.interval_minutes = "99999999999".into()).contains("ganze Zahl"));
        assert!(err(&|i| i.away_minutes = "".into()).contains("Weg-Schwelle"));
        assert!(err(&|i| i.hours_per_day = "viel".into()).contains("Arbeitszeit pro Tag"));
        assert!(err(&|i| i.hours_per_day = "25".into()).contains("zwischen 0 und 24"));
        assert!(err(&|i| i.hours_per_day = "NaN".into()).contains("zwischen 0 und 24"));
        assert!(err(&|i| i.work_days.clear()).contains("Arbeitstag"));
        assert!(err(&|i| i.work_blocks.clear()).contains("Arbeitszeit"));
        assert!(err(&|i| i.work_blocks[0].start = "".into()).contains("keine gültige Zeit"));
        assert!(err(&|i| i.work_blocks[1].start = "11:00".into()).contains("überschneiden"));
        assert!(err(&|i| i.git_emails = vec!["keine-mail".into()]).contains("E-Mail"));
        assert!(err(&|i| i.location = " ".into()).contains("Ort"));
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
