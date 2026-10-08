use crate::settings::{DayKind, Plan, Settings, clean_ort_name};
use crate::{log, store};
use chrono::{DateTime, Datelike, Days, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MAX_TEXT: usize = 10_000;
pub const MAX_HOURS: f64 = 24.0;
const MAX_RANGE_DAYS: i64 = 366 * 20;
/// Marker im Wochen-Ordner: der Import der alten JSONL-Dateien ist erledigt
const IMPORT_MARKER: &str = ".legacy-import-done";

/// Woher ein Eintrag kommt. Später kommt `kalender` dazu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quelle {
    Manuell,
    Commit,
}

/// Ein Eintrag des Journals: Text, Dauer in Stunden (auf 0.1 gerundet) und Zeitpunkt (UTC).
/// Commits tragen zusätzlich Repo und Hash, damit Git sie nicht doppelt importiert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub t: DateTime<Utc>,
    pub text: String,
    #[serde(default)]
    pub stunden: f64,
    pub quelle: Quelle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// Wochentexte: Rückblick, Reflexion und Stimmung
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeekTexts {
    pub rueckblick: String,
    pub reflexion: String,
    pub stimmung: String,
}

impl WeekTexts {
    pub fn is_empty(&self) -> bool {
        [&self.rueckblick, &self.reflexion, &self.stimmung].iter().all(|t| t.trim().is_empty())
    }

    fn normalize(&mut self) {
        for text in [&mut self.rueckblick, &mut self.reflexion, &mut self.stimmung] {
            *text = text.trim().to_string();
        }
    }

    fn validate(&self) -> Result<(), String> {
        check_text("Wochenrückblick", &self.rueckblick)?;
        check_text("Reflexion", &self.reflexion)?;
        check_text("Stimmung", &self.stimmung)
    }
}

/// Ort eines Tages, wenn er vom Wochenplan abweicht
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayInfo {
    pub ort: String,
    pub abweichend: bool,
}

/// Inhalt einer Wochen-Datei (`2026-KW30.json`). Die Tage heissen `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeekFile {
    pub jahr: i32,
    pub kw: u32,
    pub tage: BTreeMap<String, DayInfo>,
    pub eintraege: Vec<Entry>,
    pub texte: WeekTexts,
}

impl Default for WeekFile {
    fn default() -> Self {
        Self { jahr: 0, kw: 0, tage: BTreeMap::new(), eintraege: Vec::new(), texte: WeekTexts::default() }
    }
}

impl WeekFile {
    fn new(jahr: i32, kw: u32) -> Self {
        Self { jahr, kw, ..Self::default() }
    }

    /// Ort-Override des Tages (nur wenn er vom Wochenplan abweicht)
    pub fn override_of(&self, date: NaiveDate) -> Option<&str> {
        self.tage.get(&day_key(date)).filter(|d| d.abweichend).map(|d| d.ort.as_str())
    }
}

/// Neue Werte für `Journal::update`
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub text: String,
    pub stunden: f64,
}

fn new_id(t: DateTime<Utc>) -> String {
    let random = RandomState::new().build_hasher().finish();
    format!("{:x}-{random:016x}", t.timestamp_millis())
}

fn local_date(t: DateTime<Utc>) -> NaiveDate {
    t.with_timezone(&Local).date_naive()
}

fn day_key(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn check_text(label: &str, text: &str) -> Result<(), String> {
    if text.chars().count() > MAX_TEXT {
        return Err(format!("{label} ist zu lang (höchstens {MAX_TEXT} Zeichen)"));
    }
    Ok(())
}

fn required(label: &str, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{label} darf nicht leer sein"));
    }
    check_text(label, text)
}

/// Rundet auf 0.1 Stunden
pub fn round_hours(hours: f64) -> f64 {
    (hours * 10.0).round() / 10.0
}

/// Trennt eine Dauer am Ende des Textes ab: `Bug gefixt 2.4`, `Bug gefixt 2,4h`.
/// Ohne Dauer (oder wenn nur eine Zahl da steht) bleibt der Text unverändert und die Dauer ist 0.
pub fn split_duration(input: &str) -> (String, f64) {
    let trimmed = input.trim();
    let Some((head, token)) = trimmed.rsplit_once(char::is_whitespace) else {
        return (trimmed.to_string(), 0.0);
    };
    let number = token.strip_suffix(['h', 'H']).unwrap_or(token).replace(',', ".");
    let plain = !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit() || c == '.')
        && number.matches('.').count() <= 1
        && !number.starts_with('.')
        && !number.ends_with('.');
    match number.parse::<f64>() {
        Ok(hours) if plain && hours <= MAX_HOURS && !head.trim().is_empty() => {
            (head.trim().to_string(), round_hours(hours))
        }
        _ => (trimmed.to_string(), 0.0),
    }
}

impl Entry {
    pub fn manuell(t: DateTime<Utc>, text: &str, stunden: f64) -> Self {
        Self {
            id: new_id(t),
            t,
            text: text.to_string(),
            stunden,
            quelle: Quelle::Manuell,
            repo: None,
            hash: None,
        }
    }

    pub fn commit(t: DateTime<Utc>, repo: &str, hash: &str, text: &str) -> Self {
        Self {
            id: new_id(t),
            t,
            text: text.to_string(),
            stunden: 0.0,
            quelle: Quelle::Commit,
            repo: Some(repo.to_string()),
            hash: Some(hash.to_string()),
        }
    }

    pub fn local_date(&self) -> NaiveDate {
        local_date(self.t)
    }

    /// Dauer in Zehntelstunden
    pub fn tenths(&self) -> i64 {
        (self.stunden * 10.0).round() as i64
    }

    /// Schneidet Leerraum an den Rändern ab und rundet die Stunden. Läuft vor `validate`.
    fn normalize(&mut self) {
        self.text = self.text.trim().to_string();
        if let Some(repo) = &mut self.repo {
            *repo = repo.trim().to_string();
        }
        if let Some(hash) = &mut self.hash {
            *hash = hash.trim().to_string();
        }
        if self.stunden.is_finite() {
            self.stunden = round_hours(self.stunden);
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("Eintrag hat keine ID".to_string());
        }
        required("Text", &self.text)?;
        if !self.stunden.is_finite() || !(0.0..=MAX_HOURS).contains(&self.stunden) {
            return Err(format!("Die Dauer muss zwischen 0 und {MAX_HOURS} Stunden liegen"));
        }
        if self.quelle == Quelle::Commit {
            required("Repo", self.repo.as_deref().unwrap_or(""))?;
            let hash = self.hash.as_deref().unwrap_or("");
            let hex = hash.chars().all(|c| c.is_ascii_hexdigit());
            if !hex || !(7..=64).contains(&hash.len()) {
                return Err("Commit-Hash ist ungültig".to_string());
            }
        }
        Ok(())
    }
}

/// ISO-8601-Kalenderwoche (Jahr, Woche)
pub fn iso_week(date: NaiveDate) -> (i32, u32) {
    let week = date.iso_week();
    (week.year(), week.week())
}

/// Montag und Sonntag der Woche, in der `date` liegt
pub fn week_bounds(date: NaiveDate) -> (NaiveDate, NaiveDate) {
    let back = u64::from(date.weekday().num_days_from_monday());
    let monday = date.checked_sub_days(Days::new(back)).unwrap_or(date);
    let sunday = monday.checked_add_days(Days::new(6)).unwrap_or(monday);
    (monday, sunday)
}

fn week_file_name(year: i32, week: u32) -> String {
    format!("{year:04}-KW{week:02}.json")
}

/// `2026-KW30.json` -> (2026, 30)
fn parse_week_file_name(name: &str) -> Option<(i32, u32)> {
    let stem = name.strip_suffix(".json")?;
    let (year, week) = stem.split_once("-KW")?;
    Some((year.parse().ok()?, week.parse().ok()?))
}

pub struct Journal {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl Journal {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, lock: Mutex::new(()) }
    }

    /// Öffnet den Ordner aus den Einstellungen und importiert beim ersten Mal die alten JSONL-Dateien
    pub fn from_settings(settings: &Settings) -> Self {
        let journal = Self::new(settings.journal_path());
        let legacy = store::data_dir().join("journal");
        match journal.import_legacy(&legacy, &settings.plan()) {
            Ok(0) => {}
            Ok(n) => log::info(&format!("journal: {n} Einträge aus {} importiert", legacy.display())),
            Err(e) => log::warn(&format!("journal: Import der alten Dateien fehlgeschlagen: {e}")),
        }
        journal
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn week_path(&self, year: i32, week: u32) -> PathBuf {
        self.dir.join(week_file_name(year, week))
    }

    /// `Ok(None)`: die Woche hat noch keine Datei. `Err`: die Datei ist unlesbar oder kaputt.
    fn read_week(&self, year: i32, week: u32) -> Result<Option<WeekFile>, String> {
        let path = self.week_path(year, week);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{} nicht lesbar: {e}", path.display())),
        };
        let mut file: WeekFile = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} ist beschädigt und wird nicht überschrieben: {e}", path.display()))?;
        file.jahr = year;
        file.kw = week;
        Ok(Some(file))
    }

    /// Wie `read_week`, aber für Leser: Fehler werden protokolliert, die Woche gilt dann als leer
    fn read_week_lenient(&self, year: i32, week: u32) -> Option<WeekFile> {
        match self.read_week(year, week) {
            Ok(file) => file,
            Err(e) => {
                log::error(&format!("journal: {e}"));
                None
            }
        }
    }

    /// Lädt die Woche zum Ändern. Eine kaputte Datei bricht ab, damit nichts verloren geht.
    fn load_for_write(&self, year: i32, week: u32) -> Result<WeekFile, String> {
        Ok(self.read_week(year, week)?.unwrap_or_else(|| WeekFile::new(year, week)))
    }

    /// Schreibt atomar: erst in eine temporäre Datei, dann umbenennen
    fn save_week(&self, file: &mut WeekFile) -> Result<(), String> {
        file.eintraege.sort_by_key(|e| e.t);
        let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
        fs::create_dir_all(&self.dir).map_err(|e| format!("Journal-Ordner nicht erstellbar: {e}"))?;
        let path = self.week_path(file.jahr, file.kw);
        let tmp = path.with_extension("json.tmp");
        let result = (|| {
            let mut out = fs::File::create(&tmp)?;
            out.write_all(json.as_bytes())?;
            out.sync_all()?;
            drop(out);
            fs::rename(&tmp, &path)
        })();
        result.map_err(|e| {
            fs::remove_file(&tmp).ok();
            format!("Journal nicht beschreibbar: {e}")
        })
    }

    /// Validiert den Eintrag und legt ihn in die Wochen-Datei seines lokalen Tages.
    /// Gibt den gespeicherten Eintrag zurück.
    pub fn add(&self, mut entry: Entry) -> Result<Entry, String> {
        entry.normalize();
        entry.validate()?;
        let (year, week) = iso_week(entry.local_date());

        let _guard = self.guard();
        let mut file = self.load_for_write(year, week)?;
        file.eintraege.push(entry.clone());
        self.save_week(&mut file)?;
        Ok(entry)
    }

    /// Alle Einträge der lokalen Tage `from` bis `to` (inklusive), nach Zeit sortiert
    pub fn list(&self, from: NaiveDate, to: NaiveDate) -> Vec<Entry> {
        if from > to || (to - from).num_days() > MAX_RANGE_DAYS {
            return Vec::new();
        }

        // Eine Woche Puffer, falls sich die Zeitzone seit dem Schreiben geändert hat
        let start = week_bounds(from).0.checked_sub_days(Days::new(7)).unwrap_or(from);
        let end = week_bounds(to).1.checked_add_days(Days::new(7)).unwrap_or(to);

        let _guard = self.guard();
        let mut entries = Vec::new();
        let mut monday = start;
        while monday <= end {
            let (year, week) = iso_week(monday);
            if let Some(file) = self.read_week_lenient(year, week) {
                entries.extend(file.eintraege.into_iter().filter(|e| (from..=to).contains(&e.local_date())));
            }
            monday = match monday.checked_add_days(Days::new(7)) {
                Some(next) => next,
                None => break,
            };
        }
        entries.sort_by_key(|e| e.t);
        entries
    }

    pub fn day(&self, date: NaiveDate) -> Vec<Entry> {
        self.list(date, date)
    }

    /// Montag bis Sonntag der Woche, in der `date` liegt
    #[cfg(test)]
    pub fn week(&self, date: NaiveDate) -> Vec<Entry> {
        let (monday, sunday) = week_bounds(date);
        self.list(monday, sunday)
    }

    /// Die ganze Wochen-Datei der Woche, in der `date` liegt (leer, wenn es keine gibt)
    pub fn week_file(&self, date: NaiveDate) -> WeekFile {
        let (year, week) = iso_week(date);
        let _guard = self.guard();
        self.read_week_lenient(year, week).unwrap_or_else(|| WeekFile::new(year, week))
    }

    /// Ort-Override des Tages (`None`: es gilt der Wochenplan)
    pub fn day_override(&self, date: NaiveDate) -> Option<String> {
        self.week_file(date).override_of(date).map(str::to_string)
    }

    /// Setzt den Ort eines Tages, der vom Wochenplan abweicht. `None` entfernt den Override.
    pub fn set_day_override(&self, date: NaiveDate, ort: Option<&str>) -> Result<(), String> {
        let (year, week) = iso_week(date);
        let ort = ort.map(clean_ort_name).filter(|o| !o.is_empty());
        if ort.is_some_and(|o| o.chars().count() > 100) {
            return Err("Der Ort ist zu lang (höchstens 100 Zeichen)".to_string());
        }

        let _guard = self.guard();
        let mut file = self.load_for_write(year, week)?;
        match ort {
            Some(ort) => {
                file.tage.insert(day_key(date), DayInfo { ort: ort.to_string(), abweichend: true });
            }
            None => {
                file.tage.remove(&day_key(date));
            }
        }
        if file.tage.is_empty() && file.eintraege.is_empty() && file.texte.is_empty() && !self.week_path(year, week).exists() {
            return Ok(());
        }
        self.save_week(&mut file)
    }

    /// Wochentexte der Woche, in der `date` liegt
    pub fn texts(&self, date: NaiveDate) -> WeekTexts {
        self.week_file(date).texte
    }

    pub fn set_texts(&self, date: NaiveDate, mut texts: WeekTexts) -> Result<WeekTexts, String> {
        texts.normalize();
        texts.validate()?;
        let (year, week) = iso_week(date);

        let _guard = self.guard();
        let mut file = self.load_for_write(year, week)?;
        file.texte = texts.clone();
        self.save_week(&mut file)?;
        Ok(texts)
    }

    pub fn update(&self, id: &str, update: Update) -> Result<Entry, String> {
        let mut result = Err("Eintrag nicht gefunden".to_string());
        self.rewrite(id, |entry| {
            let mut new = Entry { text: update.text.clone(), stunden: update.stunden, ..entry };
            new.normalize();
            new.validate()?;
            result = Ok(new.clone());
            Ok(Some(new))
        })?;
        result
    }

    /// Entfernt einen Eintrag. Die Oberfläche bietet das für Commits nicht an,
    /// weil der Git-Import sie sonst wieder einträgt.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.rewrite(id, |_| Ok(None))
    }

    /// Sucht `id` in allen Wochen-Dateien und ersetzt den Eintrag (`Some`) oder entfernt ihn (`None`)
    fn rewrite<F>(&self, id: &str, mut f: F) -> Result<(), String>
    where
        F: FnMut(Entry) -> Result<Option<Entry>, String>,
    {
        let _guard = self.guard();
        let files = fs::read_dir(&self.dir).map_err(|_| "Eintrag nicht gefunden".to_string())?;
        let mut weeks: Vec<(i32, u32)> =
            files.flatten().filter_map(|f| parse_week_file_name(&f.file_name().to_string_lossy())).collect();
        weeks.sort_unstable();
        for (year, week) in weeks {
            // Kaputte Dateien überspringen, sie werden nie überschrieben
            let Ok(Some(mut file)) = self.read_week(year, week) else { continue };
            let Some(pos) = file.eintraege.iter().position(|e| e.id == id) else { continue };

            let entry = file.eintraege.remove(pos);
            if let Some(new) = f(entry)? {
                file.eintraege.push(new);
            }
            return self.save_week(&mut file);
        }
        Err("Eintrag nicht gefunden".to_string())
    }

    /// Einmaliger Import der alten Monats-Dateien (`2026-07.jsonl`) aus `legacy_dir`.
    /// Notizen und Commits werden Einträge ohne Dauer, Tagesmarkierungen Ort-Overrides (nur wenn sie vom
    /// Wochenplan abweichen), die letzte Reflexion einer Woche die Wochentexte (nur wenn dort noch nichts steht).
    /// Danach liegt ein Marker im Wochen-Ordner, die alten Dateien bleiben unangetastet.
    /// Gibt die Zahl der importierten Einträge zurück.
    pub fn import_legacy(&self, legacy_dir: &Path, plan: &Plan) -> Result<usize, String> {
        let _guard = self.guard();
        if self.dir.join(IMPORT_MARKER).exists() {
            return Ok(0);
        }
        let Ok(files) = fs::read_dir(legacy_dir) else { return Ok(0) };
        let mut paths: Vec<PathBuf> = files
            .flatten()
            .map(|f| f.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect();
        if paths.is_empty() {
            return Ok(0);
        }
        paths.sort();

        let mut legacy = Vec::new();
        for path in paths {
            let bytes = fs::read(&path).map_err(|e| format!("{} nicht lesbar: {e}", path.display()))?;
            for (n, line) in bytes.split(|&b| b == b'\n').enumerate() {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                match serde_json::from_slice::<LegacyEntry>(line) {
                    Ok(entry) => legacy.push(entry),
                    Err(e) => log::warn(&format!("journal: Zeile {} in {} übersprungen: {e}", n + 1, path.display())),
                }
            }
        }
        legacy.sort_by_key(LegacyEntry::t);

        // Pro Woche sammeln: Einträge, letzte Tagesmarkierung pro Tag, letzte Reflexion
        let mut weeks: BTreeMap<(i32, u32), Imported> = BTreeMap::new();
        for entry in legacy {
            let week = iso_week(local_date(entry.t()));
            let slot = weeks.entry(week).or_default();
            match entry {
                LegacyEntry::Note { id, t, text, .. } => slot.entries.push(Entry { id, ..Entry::manuell(t, &text, 0.0) }),
                LegacyEntry::Commit { id, t, repo, hash, text } => {
                    slot.entries.push(Entry { id, ..Entry::commit(t, &repo, &hash, &text) });
                }
                LegacyEntry::Day { t, kind, .. } => {
                    slot.days.insert(local_date(t), kind.into());
                }
                LegacyEntry::Reflection { review, reflection, mood, .. } => {
                    slot.texts = Some(WeekTexts { rueckblick: review, reflexion: reflection, stimmung: mood });
                }
            }
        }

        let mut imported = 0;
        for ((year, week), slot) in weeks {
            let mut file = self.load_for_write(year, week)?;
            let known: HashSet<String> = file.eintraege.iter().map(|e| e.id.clone()).collect();
            for mut entry in slot.entries {
                entry.normalize();
                if known.contains(&entry.id) {
                    continue;
                }
                match entry.validate() {
                    Ok(()) => {
                        file.eintraege.push(entry);
                        imported += 1;
                    }
                    Err(e) => log::warn(&format!("journal: alter Eintrag {} übersprungen: {e}", entry.id)),
                }
            }
            for (date, kind) in slot.days {
                let Some(ort) = kind.and_then(|k| plan.first_of(k)) else { continue };
                let planned = plan.planned(date).map(|o| o.name.as_str());
                if planned.is_some_and(|p| p.eq_ignore_ascii_case(&ort.name)) || file.tage.contains_key(&day_key(date)) {
                    continue;
                }
                file.tage.insert(day_key(date), DayInfo { ort: ort.name.clone(), abweichend: true });
            }
            if let Some(mut texts) = slot.texts
                && file.texte.is_empty()
            {
                texts.normalize();
                if texts.validate().is_ok() {
                    file.texte = texts;
                }
            }
            self.save_week(&mut file)?;
        }

        fs::create_dir_all(&self.dir).map_err(|e| format!("Journal-Ordner nicht erstellbar: {e}"))?;
        fs::write(self.dir.join(IMPORT_MARKER), "Import der alten .jsonl-Dateien erledigt\n")
            .map_err(|e| format!("Import-Marker nicht schreibbar: {e}"))?;
        Ok(imported)
    }
}

#[derive(Default)]
struct Imported {
    entries: Vec<Entry>,
    /// `None`: Markierung ohne passenden Ort (Krank)
    days: BTreeMap<NaiveDate, Option<DayKind>>,
    texts: Option<WeekTexts>,
}

/// Art der Tagesmarkierung in den alten Dateien
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LegacyKind {
    Arbeit,
    Schule,
    Uek,
    Ferien,
    Krank,
}

impl From<LegacyKind> for Option<DayKind> {
    fn from(kind: LegacyKind) -> Self {
        match kind {
            LegacyKind::Arbeit => Some(DayKind::Arbeit),
            LegacyKind::Schule => Some(DayKind::Schule),
            LegacyKind::Uek => Some(DayKind::Uek),
            LegacyKind::Ferien => Some(DayKind::Ferien),
            LegacyKind::Krank => None,
        }
    }
}

/// Zeile einer alten `.jsonl`-Datei
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum LegacyEntry {
    Note {
        id: String,
        t: DateTime<Utc>,
        text: String,
        #[allow(dead_code)]
        source: Option<String>,
    },
    Commit { id: String, t: DateTime<Utc>, repo: String, hash: String, text: String },
    Day { t: DateTime<Utc>, kind: LegacyKind },
    Reflection { t: DateTime<Utc>, review: String, reflection: String, mood: String },
}

impl LegacyEntry {
    fn t(&self) -> DateTime<Utc> {
        match self {
            Self::Note { t, .. } | Self::Commit { t, .. } | Self::Day { t, .. } | Self::Reflection { t, .. } => *t,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap().with_timezone(&Utc)
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn journal() -> (TempDir, Journal) {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::new(dir.path().join("weeks"));
        (dir, journal)
    }

    fn note(t: DateTime<Utc>, text: &str) -> Entry {
        Entry::manuell(t, text, 0.0)
    }

    fn texts_of(entries: Vec<Entry>) -> Vec<String> {
        entries.into_iter().map(|e| e.text).collect()
    }

    const HASH: &str = "3f2a9c1d7e5b4a6c8d0e1f2a3b4c5d6e7f8a9b0c";

    #[test]
    fn add_and_list_entries() {
        let (_dir, j) = journal();
        let t = at(2026, 10, 7, 9, 0);
        j.add(Entry::manuell(t, "Formular gebaut", 2.4)).unwrap();
        j.add(Entry::commit(t, "notify", HASH, "fix tray")).unwrap();

        let entries = j.day(date(2026, 10, 7));
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].quelle, entries[0].stunden), (Quelle::Manuell, 2.4));
        assert_eq!(entries[1].quelle, Quelle::Commit);
        assert_eq!(entries[1].repo.as_deref(), Some("notify"));
    }

    #[test]
    fn json_shape_matches_spec() {
        let t = at(2026, 10, 7, 9, 0);
        let json = serde_json::to_value(Entry::manuell(t, "x", 2.4)).unwrap();
        assert_eq!(json["quelle"], "manuell");
        assert_eq!(json["stunden"], 2.4);
        assert_eq!(json["text"], "x");
        assert!(json["id"].is_string());
        assert!(json["t"].as_str().unwrap().ends_with('Z'));
        assert!(json.get("repo").is_none());

        let json = serde_json::to_value(Entry::commit(t, "notify", HASH, "msg")).unwrap();
        assert_eq!(json["quelle"], "commit");
        assert_eq!(json["repo"], "notify");
    }

    #[test]
    fn one_file_per_calendar_week() {
        let (_dir, j) = journal();
        // 2026-09-27 ist ein Sonntag (KW39), 09-28 ein Montag (KW40)
        j.add(note(at(2026, 9, 27, 10, 0), "a")).unwrap();
        j.add(note(at(2026, 9, 28, 10, 0), "b")).unwrap();
        j.add(note(at(2026, 9, 30, 10, 0), "c")).unwrap();

        let kw39: WeekFile = serde_json::from_str(&fs::read_to_string(j.dir().join("2026-KW39.json")).unwrap()).unwrap();
        let kw40: WeekFile = serde_json::from_str(&fs::read_to_string(j.dir().join("2026-KW40.json")).unwrap()).unwrap();
        assert_eq!((kw39.jahr, kw39.kw, kw39.eintraege.len()), (2026, 39, 1));
        assert_eq!((kw40.jahr, kw40.kw, kw40.eintraege.len()), (2026, 40, 2));
        // keine Reste der atomaren Schreibweise
        let names: Vec<_> = fs::read_dir(j.dir()).unwrap().flatten().map(|f| f.file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }

    #[test]
    fn week_is_derived_from_local_time() {
        let (_dir, j) = journal();
        // Sonntag 23:30 lokal gehört noch in die alte Woche, egal was UTC sagt
        let sunday = at(2026, 10, 4, 23, 30);
        let monday = at(2026, 10, 5, 0, 30);
        j.add(note(sunday, "So")).unwrap();
        j.add(note(monday, "Mo")).unwrap();
        assert!(j.dir().join("2026-KW40.json").exists());
        assert!(j.dir().join("2026-KW41.json").exists());
        assert_eq!(texts_of(j.week(date(2026, 10, 4))), ["So"]);
        assert_eq!(texts_of(j.week(date(2026, 10, 5))), ["Mo"]);
    }

    #[test]
    fn rejects_empty_and_invalid() {
        let (_dir, j) = journal();
        let t = at(2026, 10, 7, 9, 0);
        assert!(j.add(note(t, "")).is_err());
        assert!(j.add(note(t, "  \n\t ")).is_err());
        assert!(j.add(note(t, &"x".repeat(MAX_TEXT + 1))).is_err());
        assert!(j.add(Entry::manuell(t, "x", -1.0)).is_err());
        assert!(j.add(Entry::manuell(t, "x", 24.1)).is_err());
        assert!(j.add(Entry::manuell(t, "x", f64::NAN)).is_err());
        assert!(j.add(Entry::commit(t, "r", "xyz", "msg")).is_err());
        assert!(j.add(Entry::commit(t, "r", HASH, "")).is_err());
        assert!(j.add(Entry::commit(t, "", HASH, "msg")).is_err());
        assert!(j.day(date(2026, 10, 7)).is_empty());
        assert!(!j.dir().join("2026-KW41.json").exists());
    }

    #[test]
    fn trims_text_and_rounds_hours() {
        let (_dir, j) = journal();
        let saved = j.add(Entry::manuell(at(2026, 10, 7, 9, 0), "  hallo \n", 2.449)).unwrap();
        assert_eq!((saved.text.as_str(), saved.stunden, saved.tenths()), ("hallo", 2.4, 24));
        assert_eq!(j.day(date(2026, 10, 7)), vec![saved]);
        let e = j.add(Entry::manuell(at(2026, 10, 7, 10, 0), "x", 0.05)).unwrap();
        assert_eq!(e.stunden, 0.1);
    }

    #[test]
    fn ids_are_unique() {
        let t = at(2026, 10, 7, 9, 0);
        let ids: HashSet<_> = (0..1000).map(|_| Entry::manuell(t, "x", 0.0).id).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn corrupt_week_is_never_overwritten() {
        let (_dir, j) = journal();
        let t = at(2026, 10, 7, 9, 0);
        j.add(note(t, "vorher")).unwrap();
        let path = j.dir().join("2026-KW41.json");
        fs::write(&path, "{ das ist kein json").unwrap();

        // Lesen: die Woche gilt als leer
        assert!(j.day(date(2026, 10, 7)).is_empty());
        // Schreiben: Fehler, Datei bleibt wie sie ist
        let err = j.add(note(t, "nachher")).unwrap_err();
        assert!(err.contains("beschädigt"), "{err}");
        assert!(j.set_texts(date(2026, 10, 7), WeekTexts { rueckblick: "x".into(), ..Default::default() }).is_err());
        assert!(j.set_day_override(date(2026, 10, 7), Some("Ferien")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ das ist kein json");
        // andere Wochen funktionieren weiter
        j.add(note(at(2026, 10, 14, 9, 0), "andere Woche")).unwrap();
    }

    #[test]
    fn missing_dir_and_files_are_empty() {
        let (_dir, j) = journal();
        assert!(j.day(date(2026, 10, 7)).is_empty());
        assert!(j.week(date(2026, 10, 7)).is_empty());
        assert_eq!(j.remove("nope"), Err("Eintrag nicht gefunden".to_string()));
        assert_eq!(j.week_file(date(2026, 10, 7)), WeekFile::new(2026, 41));
        assert!(j.texts(date(2026, 10, 7)).is_empty());
        assert_eq!(j.day_override(date(2026, 10, 7)), None);
    }

    #[test]
    fn day_and_week_ranges() {
        let (_dir, j) = journal();
        // 2026-09-28 ist ein Montag, 2026-10-04 der Sonntag danach
        for (m, d) in [(9, 27), (9, 28), (9, 30), (10, 4), (10, 5)] {
            j.add(note(at(2026, m, d, 12, 0), &format!("{m}-{d}"))).unwrap();
        }
        assert_eq!(texts_of(j.week(date(2026, 10, 1))), ["9-28", "9-30", "10-4"]);
        assert_eq!(texts_of(j.week(date(2026, 9, 28))), ["9-28", "9-30", "10-4"]);
        assert_eq!(texts_of(j.week(date(2026, 10, 4))), ["9-28", "9-30", "10-4"]);
        assert_eq!(texts_of(j.week(date(2026, 10, 5))), ["10-5"]);
        assert_eq!(texts_of(j.day(date(2026, 9, 30))), ["9-30"]);
        assert_eq!(texts_of(j.list(date(2026, 9, 27), date(2026, 9, 28))), ["9-27", "9-28"]);
        assert!(j.list(date(2026, 10, 5), date(2026, 10, 1)).is_empty());
    }

    #[test]
    fn iso_week_rules() {
        assert_eq!(week_bounds(date(2026, 10, 7)), (date(2026, 10, 5), date(2026, 10, 11)));
        assert_eq!(week_bounds(date(2026, 10, 5)), (date(2026, 10, 5), date(2026, 10, 11)));
        assert_eq!(week_bounds(date(2026, 10, 11)), (date(2026, 10, 5), date(2026, 10, 11)));
        assert_eq!(iso_week(date(2026, 10, 7)), (2026, 41));
        // Jahreswechsel
        assert_eq!(iso_week(date(2024, 12, 30)), (2025, 1));
        assert_eq!(iso_week(date(2027, 1, 1)), (2026, 53));
        assert_eq!(week_bounds(date(2027, 1, 1)), (date(2026, 12, 28), date(2027, 1, 3)));
        assert_eq!(week_file_name(2026, 3), "2026-KW03.json");
        assert_eq!(parse_week_file_name("2026-KW30.json"), Some((2026, 30)));
        assert_eq!(parse_week_file_name("2026-07.jsonl"), None);
        assert_eq!(parse_week_file_name("2026-KW30.json.tmp"), None);
    }

    #[test]
    fn week_across_year_boundary() {
        let (_dir, j) = journal();
        j.add(note(at(2026, 12, 31, 12, 0), "alt")).unwrap();
        j.add(note(at(2027, 1, 1, 12, 0), "neu")).unwrap();
        // beide liegen in KW53 des Jahres 2026
        assert!(j.dir().join("2026-KW53.json").exists());
        assert!(!j.dir().join("2027-KW01.json").exists());
        assert_eq!(j.week(date(2027, 1, 2)).len(), 2);
    }

    #[test]
    fn day_override_roundtrip() {
        let (_dir, j) = journal();
        let d = date(2026, 10, 7);
        assert_eq!(j.day_override(d), None);
        j.set_day_override(d, Some(" @Ferien ")).unwrap();
        assert_eq!(j.day_override(d), Some("Ferien".to_string()));
        assert_eq!(j.day_override(date(2026, 10, 8)), None);
        let file = j.week_file(d);
        assert_eq!(file.tage["2026-10-07"], DayInfo { ort: "Ferien".into(), abweichend: true });
        assert_eq!(file.override_of(d), Some("Ferien"));

        // Override in der Woche vorher beeinflusst diese Woche nicht
        assert_eq!(j.day_override(date(2026, 9, 30)), None);

        j.set_day_override(d, None).unwrap();
        assert_eq!(j.day_override(d), None);
        assert!(j.set_day_override(d, Some(&"x".repeat(101))).is_err());
        // Entfernen in einer Woche ohne Datei legt keine Datei an
        j.set_day_override(date(2026, 3, 4), None).unwrap();
        assert!(!j.dir().join("2026-KW10.json").exists());
    }

    #[test]
    fn week_texts_roundtrip() {
        let (_dir, j) = journal();
        let d = date(2026, 10, 7);
        j.add(note(at(2026, 10, 7, 9, 0), "x")).unwrap();
        let saved = j
            .set_texts(d, WeekTexts { rueckblick: " viel gelernt ".into(), reflexion: "mehr testen".into(), stimmung: "gut".into() })
            .unwrap();
        assert_eq!(saved.rueckblick, "viel gelernt");
        assert_eq!(j.texts(date(2026, 10, 9)), saved);
        // die Einträge bleiben
        assert_eq!(j.day(d).len(), 1);
        assert!(j.set_texts(d, WeekTexts { stimmung: "x".repeat(MAX_TEXT + 1), ..Default::default() }).is_err());
        j.set_texts(d, WeekTexts::default()).unwrap();
        assert!(j.texts(d).is_empty());
    }

    #[test]
    fn update_changes_text_and_hours() {
        let (_dir, j) = journal();
        let t = at(2026, 10, 7, 9, 0);
        let a = j.add(Entry::manuell(t, "alt", 1.0)).unwrap();
        let c = j.add(Entry::commit(t, "r", HASH, "msg")).unwrap();

        let new = j.update(&a.id, Update { text: " neu ".into(), stunden: 2.46 }).unwrap();
        assert_eq!((new.text.as_str(), new.stunden, new.quelle), ("neu", 2.5, Quelle::Manuell));
        // ein Commit lässt sich übernehmen: Dauer setzen, Quelle bleibt
        let adopted = j.update(&c.id, Update { text: "msg".into(), stunden: 0.5 }).unwrap();
        assert_eq!((adopted.quelle, adopted.stunden, adopted.hash.as_deref()), (Quelle::Commit, 0.5, Some(HASH)));

        let entries = j.day(date(2026, 10, 7));
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.text == "neu" && e.stunden == 2.5));
    }

    #[test]
    fn update_rejects_bad_input() {
        let (_dir, j) = journal();
        let a = j.add(Entry::manuell(at(2026, 10, 7, 9, 0), "alt", 1.0)).unwrap();
        assert!(j.update(&a.id, Update { text: "  ".into(), stunden: 1.0 }).is_err());
        assert!(j.update(&a.id, Update { text: "x".into(), stunden: 30.0 }).is_err());
        assert!(j.update(&a.id, Update { text: "x".into(), stunden: -1.0 }).is_err());
        assert_eq!(j.update("nope", Update { text: "x".into(), stunden: 1.0 }), Err("Eintrag nicht gefunden".into()));
        assert_eq!(j.day(date(2026, 10, 7)), vec![a]);
    }

    #[test]
    fn remove_works_across_weeks_and_keeps_the_rest() {
        let (_dir, j) = journal();
        let old = j.add(note(at(2026, 8, 3, 9, 0), "alt")).unwrap();
        j.add(note(at(2026, 10, 7, 9, 0), "neu")).unwrap();
        j.set_texts(date(2026, 8, 3), WeekTexts { stimmung: "gut".into(), ..Default::default() }).unwrap();
        j.remove(&old.id).unwrap();
        assert!(j.day(date(2026, 8, 3)).is_empty());
        assert_eq!(j.texts(date(2026, 8, 3)).stimmung, "gut");
        assert_eq!(j.day(date(2026, 10, 7)).len(), 1);
        assert_eq!(j.remove(&old.id), Err("Eintrag nicht gefunden".into()));
    }

    #[test]
    fn splits_duration_from_input() {
        assert_eq!(split_duration("Bug gefixt 2.4"), ("Bug gefixt".to_string(), 2.4));
        assert_eq!(split_duration("Bug gefixt 2,4h"), ("Bug gefixt".to_string(), 2.4));
        assert_eq!(split_duration("  Meeting   1  "), ("Meeting".to_string(), 1.0));
        assert_eq!(split_duration("Zeile 1\nZeile 2 0.5h"), ("Zeile 1\nZeile 2".to_string(), 0.5));
        assert_eq!(split_duration("Rundung 1.26"), ("Rundung".to_string(), 1.3));
        // keine Dauer
        assert_eq!(split_duration("Nur Text"), ("Nur Text".to_string(), 0.0));
        assert_eq!(split_duration("2.4"), ("2.4".to_string(), 0.0));
        assert_eq!(split_duration("Fehler 99"), ("Fehler 99".to_string(), 0.0));
        assert_eq!(split_duration("Version 1.2.3"), ("Version 1.2.3".to_string(), 0.0));
        assert_eq!(split_duration("Bug .5"), ("Bug .5".to_string(), 0.0));
        assert_eq!(split_duration("Bug -2"), ("Bug -2".to_string(), 0.0));
        assert_eq!(split_duration(""), (String::new(), 0.0));
    }

    fn legacy_line(kind: &str, extra: &str, t: &str) -> String {
        format!("{{\"id\":\"{kind}-{t}\",\"t\":\"{t}\",\"type\":\"{kind}\",{extra}}}\n")
    }

    /// Alte Monatsdatei: Mo 5.10. Notiz und Commit, Do 8.10. als Arbeit markiert, Di 6.10. Ferien, Krank am Mi,
    /// Reflexion am Fr, dazu eine kaputte Zeile
    fn write_legacy(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        let t = |d: u32, h: u32| at(2026, 10, d, h, 0).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut text = String::new();
        text += &legacy_line("note", "\"text\":\"Formular gebaut\",\"source\":\"checkin\"", &t(5, 10));
        text += &legacy_line("commit", &format!("\"repo\":\"notify\",\"hash\":\"{HASH}\",\"text\":\"fix tray\""), &t(5, 11));
        text += &legacy_line("day", "\"kind\":\"ferien\"", &t(6, 7));
        text += &legacy_line("day", "\"kind\":\"krank\"", &t(7, 7));
        text += &legacy_line("day", "\"kind\":\"arbeit\"", &t(8, 7));
        text += &legacy_line("day", "\"kind\":\"arbeit\"", &t(5, 7));
        text += "das ist kein json\n";
        text += &legacy_line("note", "\"text\":\"\",\"source\":\"quick\"", &t(5, 12));
        text += &legacy_line("reflection", "\"review\":\"alt\",\"reflection\":\"alt\",\"mood\":\"alt\"", &t(9, 15));
        text += &legacy_line("reflection", "\"review\":\"Rückblick\",\"reflection\":\"Reflexion\",\"mood\":\"gut\"", &t(9, 16));
        fs::write(dir.join("2026-10.jsonl"), text).unwrap();
    }

    #[test]
    fn legacy_jsonl_is_imported_once() {
        let (dir, j) = journal();
        let legacy = dir.path().join("journal");
        write_legacy(&legacy);
        let plan = Settings::default().plan();

        let n = j.import_legacy(&legacy, &plan).unwrap();
        assert_eq!(n, 2);

        let monday = j.day(date(2026, 10, 5));
        assert_eq!(monday.len(), 2);
        assert_eq!((monday[0].text.as_str(), monday[0].stunden, monday[0].quelle), ("Formular gebaut", 0.0, Quelle::Manuell));
        assert_eq!((monday[1].quelle, monday[1].repo.as_deref()), (Quelle::Commit, Some("notify")));

        // Ferien am Dienstag und Arbeit am Donnerstag weichen vom Plan ab; Arbeit am Montag und Krank nicht
        assert_eq!(j.day_override(date(2026, 10, 6)), Some("Ferien".to_string()));
        assert_eq!(j.day_override(date(2026, 10, 8)), Some("Noser Young".to_string()));
        assert_eq!(j.day_override(date(2026, 10, 5)), None);
        assert_eq!(j.day_override(date(2026, 10, 7)), None);

        // die letzte Reflexion der Woche gewinnt
        let texts = j.texts(date(2026, 10, 7));
        assert_eq!((texts.rueckblick.as_str(), texts.reflexion.as_str(), texts.stimmung.as_str()), ("Rückblick", "Reflexion", "gut"));

        // zweiter Lauf: nichts mehr, nichts doppelt, alte Datei unangetastet
        assert_eq!(j.import_legacy(&legacy, &plan).unwrap(), 0);
        assert_eq!(j.day(date(2026, 10, 5)).len(), 2);
        assert!(legacy.join("2026-10.jsonl").exists());
        assert!(j.dir().join(IMPORT_MARKER).exists());
    }

    #[test]
    fn legacy_import_without_old_files_does_nothing() {
        let (dir, j) = journal();
        let plan = Settings::default().plan();
        assert_eq!(j.import_legacy(&dir.path().join("gibt-es-nicht"), &plan).unwrap(), 0);
        let empty = dir.path().join("leer");
        fs::create_dir_all(&empty).unwrap();
        assert_eq!(j.import_legacy(&empty, &plan).unwrap(), 0);
        assert!(!j.dir().join(IMPORT_MARKER).exists());
    }

    #[test]
    fn legacy_import_keeps_existing_week_data() {
        let (dir, j) = journal();
        let legacy = dir.path().join("journal");
        write_legacy(&legacy);
        j.add(note(at(2026, 10, 5, 9, 0), "schon da")).unwrap();
        j.set_texts(date(2026, 10, 5), WeekTexts { stimmung: "eigene".into(), ..Default::default() }).unwrap();

        j.import_legacy(&legacy, &Settings::default().plan()).unwrap();
        assert_eq!(j.day(date(2026, 10, 5)).len(), 3);
        // vorhandene Wochentexte werden nicht überschrieben
        assert_eq!(j.texts(date(2026, 10, 5)).stimmung, "eigene");
    }
}
