use crate::{log, store};
use chrono::{DateTime, Datelike, Days, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::RandomState;
use std::fs::{self, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const MAX_TEXT: usize = 10_000;
const MAX_RANGE_DAYS: i64 = 366 * 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DayKind {
    Arbeit,
    Schule,
    Uek,
    Ferien,
    Krank,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Entry {
    Note { id: String, t: DateTime<Utc>, text: String, source: String },
    Commit { id: String, t: DateTime<Utc>, repo: String, hash: String, text: String },
    Day { id: String, t: DateTime<Utc>, kind: DayKind },
    Reflection { id: String, t: DateTime<Utc>, review: String, reflection: String, mood: String },
}

/// Neue Werte für `Journal::update`. Commits und Tagesmarkierungen sind nicht änderbar.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Note { text: String },
    Reflection { review: String, reflection: String, mood: String },
}

fn new_id(t: DateTime<Utc>) -> String {
    let random = RandomState::new().build_hasher().finish();
    format!("{:x}-{random:016x}", t.timestamp_millis())
}

fn local_date(t: DateTime<Utc>) -> NaiveDate {
    t.with_timezone(&Local).date_naive()
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

impl Entry {
    pub fn note(t: DateTime<Utc>, text: &str, source: &str) -> Self {
        Self::Note { id: new_id(t), t, text: text.to_string(), source: source.to_string() }
    }

    pub fn commit(t: DateTime<Utc>, repo: &str, hash: &str, text: &str) -> Self {
        Self::Commit {
            id: new_id(t),
            t,
            repo: repo.to_string(),
            hash: hash.to_string(),
            text: text.to_string(),
        }
    }

    pub fn day(t: DateTime<Utc>, kind: DayKind) -> Self {
        Self::Day { id: new_id(t), t, kind }
    }

    pub fn reflection(t: DateTime<Utc>, review: &str, reflection: &str, mood: &str) -> Self {
        Self::Reflection {
            id: new_id(t),
            t,
            review: review.to_string(),
            reflection: reflection.to_string(),
            mood: mood.to_string(),
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Note { id, .. }
            | Self::Commit { id, .. }
            | Self::Day { id, .. }
            | Self::Reflection { id, .. } => id,
        }
    }

    pub fn t(&self) -> DateTime<Utc> {
        match self {
            Self::Note { t, .. }
            | Self::Commit { t, .. }
            | Self::Day { t, .. }
            | Self::Reflection { t, .. } => *t,
        }
    }

    pub fn local_date(&self) -> NaiveDate {
        local_date(self.t())
    }

    /// Schneidet Leerraum an den Rändern ab. Läuft vor `validate`.
    fn normalize(&mut self) {
        match self {
            Self::Note { text, source, .. } => {
                *text = text.trim().to_string();
                *source = source.trim().to_string();
            }
            Self::Commit { repo, hash, text, .. } => {
                *repo = repo.trim().to_string();
                *hash = hash.trim().to_string();
                *text = text.trim().to_string();
            }
            Self::Day { .. } => {}
            Self::Reflection { review, reflection, mood, .. } => {
                *review = review.trim().to_string();
                *reflection = reflection.trim().to_string();
                *mood = mood.trim().to_string();
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id().is_empty() {
            return Err("Eintrag hat keine ID".to_string());
        }
        match self {
            Self::Note { text, source, .. } => {
                required("Text", text)?;
                required("Quelle", source)
            }
            Self::Commit { repo, hash, text, .. } => {
                required("Repo", repo)?;
                required("Text", text)?;
                let hex = hash.chars().all(|c| c.is_ascii_hexdigit());
                if !hex || !(7..=64).contains(&hash.len()) {
                    return Err("Commit-Hash ist ungültig".to_string());
                }
                Ok(())
            }
            Self::Day { .. } => Ok(()),
            Self::Reflection { review, reflection, mood, .. } => {
                if review.trim().is_empty() && reflection.trim().is_empty() {
                    return Err("Wochenrückblick oder Reflexion muss ausgefüllt sein".to_string());
                }
                check_text("Wochenrückblick", review)?;
                check_text("Reflexion", reflection)?;
                check_text("Stimmung", mood)
            }
        }
    }

    fn apply(self, update: Update) -> Result<Self, String> {
        let mut entry = match (self, update) {
            (Self::Note { id, t, source, .. }, Update::Note { text }) => {
                Self::Note { id, t, text, source }
            }
            (
                Self::Reflection { id, t, .. },
                Update::Reflection { review, reflection, mood },
            ) => Self::Reflection { id, t, review, reflection, mood },
            _ => return Err("Dieser Eintrag lässt sich so nicht ändern".to_string()),
        };
        entry.normalize();
        entry.validate()?;
        Ok(entry)
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

/// Die zuletzt gesetzte Tagesmarkierung (`entries` nach Zeit sortiert)
pub fn last_day_kind(entries: &[Entry]) -> Option<DayKind> {
    entries.iter().rev().find_map(|e| match e {
        Entry::Day { kind, .. } => Some(*kind),
        _ => None,
    })
}

fn month_file(year: i32, month: u32) -> String {
    format!("{year:04}-{month:02}.jsonl")
}

fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|&b| b == b'\n').map(|l| l.strip_suffix(b"\r").unwrap_or(l))
}

fn parse_line(line: &[u8]) -> Result<Entry, String> {
    let entry: Entry = serde_json::from_slice(line).map_err(|e| e.to_string())?;
    entry.validate()?;
    Ok(entry)
}

fn read_entries(path: &Path) -> Vec<Entry> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            log::error(&format!("journal: {} nicht lesbar: {e}", path.display()));
            return Vec::new();
        }
    };

    let mut entries = Vec::new();
    for (n, line) in lines(&bytes).enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match parse_line(line) {
            Ok(entry) => entries.push(entry),
            Err(e) => log::warn(&format!(
                "journal: Zeile {} in {} übersprungen: {e}",
                n + 1,
                path.display()
            )),
        }
    }
    entries
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().read(true).append(true).create(true).open(path)?;
    let mut text = String::new();
    if file.metadata()?.len() > 0 {
        // Eine halb geschriebene letzte Zeile darf den neuen Eintrag nicht verschlucken
        let mut last = [0u8; 1];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            text.push('\n');
        }
    }
    text.push_str(line);
    text.push('\n');
    file.write_all(text.as_bytes())
}

pub struct Journal {
    dir: PathBuf,
    lock: Mutex<()>,
}

impl Journal {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, lock: Mutex::new(()) }
    }

    pub fn open() -> Self {
        Self::new(store::data_dir().join("journal"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Validiert den Eintrag und hängt ihn an die Monatsdatei an. Gibt den gespeicherten Eintrag zurück.
    pub fn add(&self, mut entry: Entry) -> Result<Entry, String> {
        entry.normalize();
        entry.validate()?;
        let line = serde_json::to_string(&entry).map_err(|e| e.to_string())?;

        let local = entry.t().with_timezone(&Local);
        let _guard = self.guard();
        fs::create_dir_all(&self.dir)
            .map_err(|e| format!("Journal-Ordner nicht erstellbar: {e}"))?;
        let path = self.dir.join(month_file(local.year(), local.month()));
        append_line(&path, &line).map_err(|e| format!("Journal nicht beschreibbar: {e}"))?;
        Ok(entry)
    }

    /// Alle Einträge der lokalen Tage `from` bis `to` (inklusive), nach Zeit sortiert
    pub fn list(&self, from: NaiveDate, to: NaiveDate) -> Vec<Entry> {
        if from > to || (to - from).num_days() > MAX_RANGE_DAYS {
            return Vec::new();
        }

        // Einen Monat Puffer, falls sich die Zeitzone seit dem Schreiben geändert hat
        let start = from.checked_sub_days(Days::new(31)).unwrap_or(from);
        let end = to.checked_add_days(Days::new(31)).unwrap_or(to);
        let (mut year, mut month) = (start.year(), start.month());

        let _guard = self.guard();
        let mut entries = Vec::new();
        while (year, month) <= (end.year(), end.month()) {
            let path = self.dir.join(month_file(year, month));
            entries.extend(
                read_entries(&path)
                    .into_iter()
                    .filter(|e| (from..=to).contains(&e.local_date())),
            );
            (year, month) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
        }
        entries.sort_by_key(Entry::t);
        entries
    }

    pub fn day(&self, date: NaiveDate) -> Vec<Entry> {
        self.list(date, date)
    }

    /// Montag bis Sonntag der Woche, in der `date` liegt
    pub fn week(&self, date: NaiveDate) -> Vec<Entry> {
        let (monday, sunday) = week_bounds(date);
        self.list(monday, sunday)
    }

    pub fn day_kind(&self, date: NaiveDate) -> Option<DayKind> {
        last_day_kind(&self.day(date))
    }

    pub fn update(&self, id: &str, update: Update) -> Result<Entry, String> {
        let mut update = Some(update);
        let mut result = Err("Eintrag nicht gefunden".to_string());
        self.rewrite(id, |entry| {
            let Some(update) = update.take() else { return Ok(Some(entry)) };
            let new = entry.apply(update)?;
            result = Ok(new.clone());
            Ok(Some(new))
        })?;
        result
    }

    /// Entfernt einen Eintrag beliebigen Typs. Die Oberfläche bietet das für Commits nicht an,
    /// weil der Git-Import sie sonst wieder einträgt.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.rewrite(id, |_| Ok(None))
    }

    /// Sucht `id` in allen Monatsdateien und ersetzt die Zeile (`Some`) oder entfernt sie (`None`).
    /// Kaputte Zeilen bleiben unverändert erhalten.
    fn rewrite<F>(&self, id: &str, mut f: F) -> Result<(), String>
    where
        F: FnMut(Entry) -> Result<Option<Entry>, String>,
    {
        let _guard = self.guard();
        let files = fs::read_dir(&self.dir).map_err(|_| "Eintrag nicht gefunden".to_string())?;
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else { continue };

            let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
            let mut found = false;
            for line in lines(&bytes) {
                if line.is_empty() {
                    continue;
                }
                let hit = match parse_line(line) {
                    Ok(entry) if entry.id() == id && !found => Some(entry),
                    _ => None,
                };
                let Some(entry) = hit else {
                    out.extend_from_slice(line);
                    out.push(b'\n');
                    continue;
                };
                found = true;
                if let Some(new) = f(entry)? {
                    let json = serde_json::to_string(&new).map_err(|e| e.to_string())?;
                    out.extend_from_slice(json.as_bytes());
                    out.push(b'\n');
                }
            }
            if !found {
                continue;
            }

            let tmp = path.with_extension("jsonl.tmp");
            fs::write(&tmp, &out).map_err(|e| format!("Journal nicht beschreibbar: {e}"))?;
            fs::rename(&tmp, &path).map_err(|e| format!("Journal nicht beschreibbar: {e}"))?;
            return Ok(());
        }
        Err("Eintrag nicht gefunden".to_string())
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
        let journal = Journal::new(dir.path().join("journal"));
        (dir, journal)
    }

    const HASH: &str = "3f2a9c1d7e5b4a6c8d0e1f2a3b4c5d6e7f8a9b0c";

    #[test]
    fn add_and_list_all_types() {
        let (_dir, j) = journal();
        let t = at(2026, 10, 7, 9, 0);
        j.add(Entry::note(t, "Formular gebaut", "checkin")).unwrap();
        j.add(Entry::commit(t, "notify", HASH, "fix tray")).unwrap();
        j.add(Entry::day(t, DayKind::Uek)).unwrap();
        j.add(Entry::reflection(t, "viel gelernt", "mehr testen", "gut")).unwrap();

        let entries = j.day(date(2026, 10, 7));
        assert_eq!(entries.len(), 4);
        assert!(matches!(entries[0], Entry::Note { .. }));
        assert!(matches!(entries[3], Entry::Reflection { .. }));
    }

    #[test]
    fn json_shape_matches_spec() {
        let t = at(2026, 10, 7, 9, 0);
        let json = serde_json::to_value(Entry::day(t, DayKind::Uek)).unwrap();
        assert_eq!(json["type"], "day");
        assert_eq!(json["kind"], "uek");
        assert!(json["id"].is_string());
        assert!(json["t"].as_str().unwrap().ends_with('Z'));

        let json = serde_json::to_value(Entry::note(t, "x", "quick")).unwrap();
        assert_eq!(json["type"], "note");
        assert_eq!(json["source"], "quick");
    }

    #[test]
    fn file_per_month_and_append_only() {
        let (_dir, j) = journal();
        j.add(Entry::note(at(2026, 9, 30, 10, 0), "a", "quick")).unwrap();
        j.add(Entry::note(at(2026, 10, 1, 10, 0), "b", "quick")).unwrap();
        j.add(Entry::note(at(2026, 10, 2, 10, 0), "c", "quick")).unwrap();

        let sept = fs::read_to_string(j.dir().join("2026-09.jsonl")).unwrap();
        let okt = fs::read_to_string(j.dir().join("2026-10.jsonl")).unwrap();
        assert_eq!(sept.lines().count(), 1);
        assert_eq!(okt.lines().count(), 2);
    }

    #[test]
    fn rejects_empty_and_invalid() {
        let (_dir, j) = journal();
        let t = at(2026, 10, 7, 9, 0);
        assert!(j.add(Entry::note(t, "", "quick")).is_err());
        assert!(j.add(Entry::note(t, "  \n\t ", "quick")).is_err());
        assert!(j.add(Entry::note(t, "x", " ")).is_err());
        assert!(j.add(Entry::note(t, &"x".repeat(MAX_TEXT + 1), "quick")).is_err());
        assert!(j.add(Entry::commit(t, "r", "xyz", "msg")).is_err());
        assert!(j.add(Entry::commit(t, "r", HASH, "")).is_err());
        assert!(j.add(Entry::commit(t, "", HASH, "msg")).is_err());
        assert!(j.add(Entry::reflection(t, "", " ", "gut")).is_err());
        assert!(j.day(date(2026, 10, 7)).is_empty());
        assert!(!j.dir().join("2026-10.jsonl").exists());
    }

    #[test]
    fn trims_text_before_saving() {
        let (_dir, j) = journal();
        let saved = j.add(Entry::note(at(2026, 10, 7, 9, 0), "  hallo \n", "quick")).unwrap();
        assert!(matches!(&saved, Entry::Note { text, .. } if text == "hallo"));
        assert_eq!(j.day(date(2026, 10, 7)), vec![saved]);
    }

    #[test]
    fn ids_are_unique() {
        let t = at(2026, 10, 7, 9, 0);
        let ids: std::collections::HashSet<_> =
            (0..1000).map(|_| Entry::note(t, "x", "quick").id().to_string()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn broken_lines_are_skipped() {
        let (_dir, j) = journal();
        let t = at(2026, 10, 7, 9, 0);
        j.add(Entry::note(t, "vorher", "quick")).unwrap();

        let path = j.dir().join("2026-10.jsonl");
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"das ist kein json\n").unwrap();
        file.write_all(b"{\"id\":\"x\",\"t\":\"2026-10-07T07:00:00Z\",\"type\":\"unbekannt\"}\n").unwrap();
        file.write_all(b"{\"id\":\"y\",\"t\":\"2026-10-07T07:00:00Z\",\"type\":\"note\",\"text\":\"\",\"source\":\"quick\"}\n").unwrap();
        file.write_all(b"\n   \n").unwrap();
        file.write_all(&[0xff, 0xfe, b'\n']).unwrap();
        file.write_all(b"{\"id\":\"z\",\"t\":\"2026-10-07T07:00:00Z\",\"type\":\"note\",\"te").unwrap();
        drop(file);

        // Auch nach halb geschriebener Zeile geht das Anhängen weiter
        j.add(Entry::note(t, "nachher", "quick")).unwrap();

        let texts: Vec<_> = j
            .day(date(2026, 10, 7))
            .into_iter()
            .filter_map(|e| match e {
                Entry::Note { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["vorher", "nachher"]);
    }

    #[test]
    fn missing_dir_and_files_are_empty() {
        let (_dir, j) = journal();
        assert!(j.day(date(2026, 10, 7)).is_empty());
        assert!(j.week(date(2026, 10, 7)).is_empty());
        assert_eq!(j.remove("nope"), Err("Eintrag nicht gefunden".to_string()));
    }

    #[test]
    fn day_and_week_ranges() {
        let (_dir, j) = journal();
        // 2026-09-28 ist ein Montag, 2026-10-04 der Sonntag danach
        for (m, d) in [(9, 27), (9, 28), (9, 30), (10, 4), (10, 5)] {
            j.add(Entry::note(at(2026, m, d, 12, 0), &format!("{m}-{d}"), "quick")).unwrap();
        }
        let texts = |entries: Vec<Entry>| -> Vec<String> {
            entries
                .into_iter()
                .filter_map(|e| match e {
                    Entry::Note { text, .. } => Some(text),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(texts(j.week(date(2026, 10, 1))), ["9-28", "9-30", "10-4"]);
        assert_eq!(texts(j.week(date(2026, 9, 28))), ["9-28", "9-30", "10-4"]);
        assert_eq!(texts(j.week(date(2026, 10, 4))), ["9-28", "9-30", "10-4"]);
        assert_eq!(texts(j.week(date(2026, 10, 5))), ["10-5"]);
        assert_eq!(texts(j.day(date(2026, 9, 30))), ["9-30"]);
        assert_eq!(texts(j.list(date(2026, 9, 27), date(2026, 9, 28))), ["9-27", "9-28"]);
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
    }

    #[test]
    fn week_across_year_boundary() {
        let (_dir, j) = journal();
        j.add(Entry::note(at(2026, 12, 31, 12, 0), "alt", "quick")).unwrap();
        j.add(Entry::note(at(2027, 1, 1, 12, 0), "neu", "quick")).unwrap();
        assert_eq!(j.week(date(2027, 1, 2)).len(), 2);
    }

    #[test]
    fn last_day_marker_wins() {
        let (_dir, j) = journal();
        assert_eq!(j.day_kind(date(2026, 10, 7)), None);
        j.add(Entry::day(at(2026, 10, 7, 7, 0), DayKind::Arbeit)).unwrap();
        j.add(Entry::day(at(2026, 10, 7, 8, 0), DayKind::Krank)).unwrap();
        j.add(Entry::note(at(2026, 10, 7, 9, 0), "x", "quick")).unwrap();
        assert_eq!(j.day_kind(date(2026, 10, 7)), Some(DayKind::Krank));
        assert_eq!(j.day_kind(date(2026, 10, 8)), None);
    }

    #[test]
    fn update_note_and_reflection() {
        let (_dir, j) = journal();
        let t = at(2026, 10, 7, 9, 0);
        let note = j.add(Entry::note(t, "alt", "checkin")).unwrap();
        let refl = j.add(Entry::reflection(t, "a", "b", "c")).unwrap();

        let new = j.update(note.id(), Update::Note { text: " neu ".into() }).unwrap();
        assert!(matches!(&new, Entry::Note { text, source, .. } if text == "neu" && source == "checkin"));
        j.update(
            refl.id(),
            Update::Reflection { review: "r".into(), reflection: "".into(), mood: "".into() },
        )
        .unwrap();

        let entries = j.day(date(2026, 10, 7));
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| matches!(e, Entry::Note { text, .. } if text == "neu")));
        assert!(entries.iter().any(|e| matches!(e, Entry::Reflection { review, .. } if review == "r")));
    }

    #[test]
    fn update_rejects_bad_input() {
        let (_dir, j) = journal();
        let t = at(2026, 10, 7, 9, 0);
        let note = j.add(Entry::note(t, "alt", "quick")).unwrap();
        let commit = j.add(Entry::commit(t, "r", HASH, "msg")).unwrap();

        assert!(j.update(note.id(), Update::Note { text: "  ".into() }).is_err());
        assert!(j.update(commit.id(), Update::Note { text: "x".into() }).is_err());
        assert!(
            j.update(
                note.id(),
                Update::Reflection { review: "a".into(), reflection: "b".into(), mood: "c".into() }
            )
            .is_err()
        );
        assert_eq!(j.update("nope", Update::Note { text: "x".into() }), Err("Eintrag nicht gefunden".into()));
        assert!(j.day(date(2026, 10, 7)).contains(&note));
    }

    #[test]
    fn update_and_remove_keep_broken_lines() {
        let (_dir, j) = journal();
        let t = at(2026, 10, 7, 9, 0);
        let a = j.add(Entry::note(t, "a", "quick")).unwrap();
        let b = j.add(Entry::note(t, "b", "quick")).unwrap();
        let path = j.dir().join("2026-10.jsonl");
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"kaputt\n").unwrap();
        drop(file);

        j.update(a.id(), Update::Note { text: "a2".into() }).unwrap();
        j.remove(b.id()).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("kaputt"));
        assert_eq!(j.day(date(2026, 10, 7)).len(), 1);
        assert_eq!(j.remove(b.id()), Err("Eintrag nicht gefunden".into()));
    }

    #[test]
    fn remove_works_across_months() {
        let (_dir, j) = journal();
        let old = j.add(Entry::note(at(2026, 8, 3, 9, 0), "alt", "quick")).unwrap();
        j.add(Entry::note(at(2026, 10, 7, 9, 0), "neu", "quick")).unwrap();
        j.remove(old.id()).unwrap();
        assert!(j.day(date(2026, 8, 3)).is_empty());
        assert_eq!(j.day(date(2026, 10, 7)).len(), 1);
    }

    #[test]
    fn crlf_lines_are_read() {
        let (_dir, j) = journal();
        let entry = Entry::note(at(2026, 10, 7, 9, 0), "x", "quick");
        fs::create_dir_all(j.dir()).unwrap();
        let line = serde_json::to_string(&entry).unwrap();
        fs::write(j.dir().join("2026-10.jsonl"), format!("{line}\r\n")).unwrap();
        assert_eq!(j.day(date(2026, 10, 7)), vec![entry]);
    }
}
