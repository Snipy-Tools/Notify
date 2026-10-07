use crate::docx;
use crate::journal::{DayKind, Entry, Journal, iso_week, week_bounds};
use crate::log;
use crate::settings::Settings;
use crate::system;
use crate::timer::{Schedule, effective_kind};
use chrono::{Datelike, Days, Local, NaiveDate, NaiveTime, Timelike, Weekday};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

pub const MAX_HOURS_PER_DAY: f64 = 24.0;
const NONE: &str = "–";

/// Geprüfte Export-Einstellungen
#[derive(Debug, Clone, PartialEq)]
pub struct ExportConfig {
    pub location: String,
    pub hours_per_day: f64,
    /// `None`: Standardordner `Dokumente\Recapr`
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DayData {
    pub date: NaiveDate,
    pub place: String,
    pub activities: Vec<String>,
    /// Gleich lang wie `activities` (leere Texte bei Zeilen ohne Zeit), oder leer
    pub times: Vec<String>,
    pub total: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WeekData {
    pub year: i32,
    pub week: u32,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub days: Vec<DayData>,
    pub review: String,
    pub reflection: String,
    pub mood: String,
}

pub fn german_weekday(date: NaiveDate) -> &'static str {
    match date.weekday() {
        Weekday::Mon => "Montag",
        Weekday::Tue => "Dienstag",
        Weekday::Wed => "Mittwoch",
        Weekday::Thu => "Donnerstag",
        Weekday::Fri => "Freitag",
        Weekday::Sat => "Samstag",
        Weekday::Sun => "Sonntag",
    }
}

/// Zehntelstunden als "8.4h"
pub fn format_hours(tenths: i64) -> String {
    format!("{}.{}h", tenths / 10, tenths % 10)
}

/// Minuten innerhalb der Arbeitszeit zwischen zwei Uhrzeiten (Minuten seit Mitternacht)
pub fn work_minutes(blocks: &[(NaiveTime, NaiveTime)], from: i64, to: i64) -> i64 {
    blocks
        .iter()
        .map(|&(start, end)| (to.min(minutes_of(end)) - from.max(minutes_of(start))).max(0))
        .sum()
}

fn minutes_of(t: NaiveTime) -> i64 {
    i64::from(t.hour() * 60 + t.minute())
}

/// Verteilt `total` Zehntelstunden im Verhältnis der Gewichte, so dass die Summe genau `total` ist
/// (Methode des grössten Rests). Sind alle Gewichte 0, wird gleichmässig verteilt.
pub fn distribute(total: i64, weights: &[i64]) -> Vec<i64> {
    if weights.is_empty() {
        return Vec::new();
    }
    let total = total.max(0);
    let weights: Vec<i64> = if weights.iter().all(|&w| w <= 0) {
        vec![1; weights.len()]
    } else {
        weights.iter().map(|&w| w.max(0)).collect()
    };
    let sum: i64 = weights.iter().sum();

    let mut parts: Vec<i64> = weights.iter().map(|&w| total * w / sum).collect();
    let remainders: Vec<i64> = weights.iter().map(|&w| total * w % sum).collect();
    let mut left = total - parts.iter().sum::<i64>();

    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by(|&a, &b| remainders[b].cmp(&remainders[a]).then(a.cmp(&b)));
    for i in order {
        if left == 0 {
            break;
        }
        parts[i] += 1;
        left -= 1;
    }
    parts
}

fn strip_bullet(line: &str) -> &str {
    let line = line.trim();
    for prefix in ["- ", "* ", "• "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    line
}

fn place_for(kind: Option<DayKind>, location: &str) -> String {
    match kind {
        Some(DayKind::Arbeit) => location.to_string(),
        Some(DayKind::Schule) => "Schule".to_string(),
        Some(DayKind::Uek) => "ÜK".to_string(),
        Some(DayKind::Ferien) => "Ferien".to_string(),
        Some(DayKind::Krank) => "Krank".to_string(),
        None => "Frei".to_string(),
    }
}

fn local_minutes(entry: &Entry) -> i64 {
    let t = entry.t().with_timezone(&Local);
    i64::from(t.hour() * 60 + t.minute())
}

fn build_day(date: NaiveDate, day: &[&Entry], schedule: &Schedule, config: &ExportConfig) -> Option<DayData> {
    let marked = day.iter().rev().find_map(|e| match e {
        Entry::Day { kind, .. } => Some(*kind),
        _ => None,
    });
    let kind = effective_kind(date, marked, schedule);
    let notes: Vec<(&Entry, &str)> = day
        .iter()
        .filter_map(|e| match e {
            Entry::Note { text, .. } => Some((*e, text.as_str())),
            _ => None,
        })
        .collect();
    let commits: Vec<&Entry> = day.iter().copied().filter(|e| matches!(e, Entry::Commit { .. })).collect();

    let weekend = matches!(date.weekday(), Weekday::Sat | Weekday::Sun);
    if weekend && kind.is_none() && notes.is_empty() && commits.is_empty() {
        return None;
    }

    // Zeit nur an Arbeitstagen (und freien Tagen mit Notizen), nicht an Schule, ÜK, Ferien oder Krank
    let timed = matches!(kind, Some(DayKind::Arbeit) | None);
    let tenths = distribution(&notes, schedule, config, timed);

    let mut activities = Vec::new();
    let mut times = Vec::new();
    let mut listed: HashSet<String> = HashSet::new();
    for (i, (_, text)) in notes.iter().enumerate() {
        let lines: Vec<&str> = text.lines().map(strip_bullet).filter(|l| !l.is_empty()).collect();
        for (n, line) in lines.iter().enumerate() {
            listed.insert((*line).to_string());
            activities.push(format!("- {line}"));
            times.push(match tenths.get(i) {
                Some(&t) if n == 0 => format_hours(t),
                _ => String::new(),
            });
        }
    }
    for entry in commits {
        if let Entry::Commit { repo, text, .. } = entry
            && !listed.contains(text.trim())
        {
            activities.push(format!("- Commit {repo}: {}", text.trim()));
            times.push(String::new());
        }
    }

    let total = if tenths.is_empty() { NONE.to_string() } else { format_hours(tenths.iter().sum()) };
    if activities.is_empty() {
        activities.push(NONE.to_string());
    }
    if tenths.is_empty() {
        times.clear();
    }

    Some(DayData { date, place: place_for(kind, &config.location), activities, times, total })
}

/// Zehntelstunden pro Notiz. Leer, wenn der Tag keine Zeit bekommt.
fn distribution(notes: &[(&Entry, &str)], schedule: &Schedule, config: &ExportConfig, timed: bool) -> Vec<i64> {
    if !timed || notes.is_empty() {
        return Vec::new();
    }
    let Some(&(first_start, _)) = schedule.blocks.first() else { return Vec::new() };

    // Gemessene Zeit: Minuten innerhalb der Arbeitszeit seit dem vorigen Eintrag
    let mut previous = minutes_of(first_start);
    let mut weights = Vec::new();
    for (entry, _) in notes {
        let now = local_minutes(entry);
        weights.push(work_minutes(&schedule.blocks, previous, now));
        previous = previous.max(now);
    }

    let target = (config.hours_per_day.clamp(0.0, MAX_HOURS_PER_DAY) * 10.0).round() as i64;
    if target > 0 {
        distribute(target, &weights)
    } else {
        // Bei 0 zählt nur die gemessene Zeit (6 Minuten = 0.1h)
        weights.iter().map(|&m| (m + 3) / 6).collect()
    }
}

/// Baut die Daten der Woche, in der `date` liegt
pub fn build_week(date: NaiveDate, entries: &[Entry], schedule: &Schedule, config: &ExportConfig) -> WeekData {
    let (monday, sunday) = week_bounds(date);
    let (year, week) = iso_week(monday);

    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by_key(|e| e.t());

    let mut days = Vec::new();
    for offset in 0..7u64 {
        let Some(day) = monday.checked_add_days(Days::new(offset)) else { continue };
        let of_day: Vec<&Entry> = sorted.iter().copied().filter(|e| e.local_date() == day).collect();
        days.extend(build_day(day, &of_day, schedule, config));
    }

    // Pro Woche gilt die zuletzt geschriebene Reflexion
    let (review, reflection, mood) = sorted
        .iter()
        .rev()
        .find_map(|e| match e {
            Entry::Reflection { review, reflection, mood, .. } => Some((review.clone(), reflection.clone(), mood.clone())),
            _ => None,
        })
        .unwrap_or_default();

    WeekData { year, week, from: monday, to: sunday, days, review, reflection, mood }
}

fn default_dir() -> PathBuf {
    system::documents_dir().unwrap_or_else(std::env::temp_dir).join("Recapr")
}

pub fn file_name(week: &WeekData) -> String {
    format!("ABJ_{}-KW{:02}.docx", week.year, week.week)
}

/// Exportiert die aktuelle (`last == false`) oder die vorige Woche und gibt den Pfad der Datei zurück
pub fn export_week(today: NaiveDate, last: bool) -> Result<PathBuf, String> {
    let settings = Settings::load();
    let schedule = settings.schedule()?;
    let config = settings.export()?;

    let date = if last { today.checked_sub_days(Days::new(7)).unwrap_or(today) } else { today };
    let journal = Journal::open();
    let week = build_week(date, &journal.week(date), &schedule, &config);

    let dir = config.dir.unwrap_or_else(default_dir);
    fs::create_dir_all(&dir).map_err(|e| format!("Exportordner {} nicht erstellbar: {e}", dir.display()))?;
    let path = dir.join(file_name(&week));
    fs::write(&path, docx::week_to_docx(&week)).map_err(|e| {
        format!("{} nicht schreibbar (ist die Datei in Word geöffnet?): {e}", path.display())
    })?;
    log::info(&format!("Export geschrieben: {}", path.display()));
    Ok(path)
}

/// Kommandozeile: `--export` (aktuelle Woche), `--export --last` (letzte Woche), `--no-open` (Explorer nicht öffnen).
/// Gibt `None` zurück, wenn `--export` fehlt, sonst den Exit-Code.
pub fn cli(args: &[String]) -> Option<i32> {
    if !args.iter().any(|a| a == "--export") {
        return None;
    }
    system::attach_parent_console();
    if let Some(unknown) = args.iter().find(|a| !["--export", "--last", "--no-open"].contains(&a.as_str())) {
        system::console_line(&format!("Unbekannte Option {unknown}. Erlaubt: --export [--last] [--no-open]"));
        return Some(2);
    }

    let last = args.iter().any(|a| a == "--last");
    match export_week(Local::now().date_naive(), last) {
        Ok(path) => {
            system::console_line(&path.display().to_string());
            if !args.iter().any(|a| a == "--no-open") {
                system::reveal_in_explorer(&path);
            }
            Some(0)
        }
        Err(e) => {
            log::error(&format!("Export fehlgeschlagen: {e}"));
            system::console_line(&format!("Export fehlgeschlagen: {e}"));
            Some(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use chrono::{DateTime, TimeZone, Utc};

    fn at(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        // Juli 2026: 20 = Montag
        Local.with_ymd_and_hms(2026, 7, d, h, m, 0).unwrap().with_timezone(&Utc)
    }

    fn date(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, d).unwrap()
    }

    fn schedule() -> Schedule {
        Settings::default().schedule().unwrap()
    }

    fn config(hours: f64) -> ExportConfig {
        ExportConfig { location: "@Noseryoung".into(), hours_per_day: hours, dir: None }
    }

    #[test]
    fn distribute_sums_exactly() {
        assert_eq!(distribute(84, &[180, 120, 60, 120]), vec![32, 21, 10, 21]);
        assert_eq!(distribute(84, &[180, 120, 60, 120]).iter().sum::<i64>(), 84);
        assert_eq!(distribute(10, &[1, 1, 1]), vec![4, 3, 3]);
        assert_eq!(distribute(84, &[0, 0, 0, 0]), vec![21, 21, 21, 21]);
        assert_eq!(distribute(84, &[0, 100]), vec![0, 84]);
        assert_eq!(distribute(84, &[5]), vec![84]);
        assert!(distribute(84, &[]).is_empty());
        assert_eq!(distribute(0, &[3, 4]), vec![0, 0]);
        assert_eq!(distribute(-5, &[3, 4]), vec![0, 0]);
    }

    #[test]
    fn distribute_never_overshoots_for_many_entries() {
        let weights: Vec<i64> = (1..=50).collect();
        let parts = distribute(240, &weights);
        assert_eq!(parts.iter().sum::<i64>(), 240);
        assert!(parts.iter().all(|&p| p >= 0));
    }

    #[test]
    fn work_minutes_skips_the_break() {
        let blocks = schedule().blocks;
        let m = |h: i64, min: i64| h * 60 + min;
        assert_eq!(work_minutes(&blocks, m(8, 0), m(11, 0)), 180);
        assert_eq!(work_minutes(&blocks, m(11, 0), m(14, 0)), 120);
        assert_eq!(work_minutes(&blocks, m(12, 0), m(13, 0)), 0);
        assert_eq!(work_minutes(&blocks, m(7, 0), m(18, 0)), 480);
        assert_eq!(work_minutes(&blocks, m(15, 0), m(15, 0)), 0);
        assert_eq!(work_minutes(&blocks, m(9, 0), m(8, 0)), 0);
    }

    #[test]
    fn formats_hours_and_weekdays() {
        assert_eq!(format_hours(84), "8.4h");
        assert_eq!(format_hours(30), "3.0h");
        assert_eq!(format_hours(0), "0.0h");
        assert_eq!(format_hours(105), "10.5h");
        assert_eq!(german_weekday(date(20)), "Montag");
        assert_eq!(german_weekday(date(26)), "Sonntag");
    }

    #[test]
    fn day_with_notes_gets_the_day_hours() {
        let entries = vec![
            Entry::note(at(20, 11, 0), "Erste", "checkin"),
            Entry::note(at(20, 14, 0), "Zweite", "checkin"),
            Entry::note(at(20, 15, 0), "Dritte", "checkin"),
            Entry::note(at(20, 17, 0), "Vierte", "checkin"),
        ];
        let week = build_week(date(20), &entries, &schedule(), &config(8.4));
        let monday = &week.days[0];
        assert_eq!(monday.place, "@Noseryoung");
        assert_eq!(monday.activities, ["- Erste", "- Zweite", "- Dritte", "- Vierte"]);
        // gemessen 3h : 2h : 1h : 2h, verteilt auf 8.4h
        assert_eq!(monday.times, ["3.2h", "2.1h", "1.0h", "2.1h"]);
        assert_eq!(monday.total, "8.4h");
    }

    #[test]
    fn zero_hours_use_measured_time_only() {
        let entries = vec![
            Entry::note(at(20, 10, 0), "A", "checkin"),
            Entry::note(at(20, 15, 0), "B", "checkin"),
        ];
        let week = build_week(date(20), &entries, &schedule(), &config(0.0));
        // 08-10 = 2.0h, 10-12 + 13-15 = 4.0h
        assert_eq!(week.days[0].times, ["2.0h", "4.0h"]);
        assert_eq!(week.days[0].total, "6.0h");
    }

    #[test]
    fn multi_line_notes_get_one_time_on_the_first_line() {
        let entries = vec![
            Entry::note(at(20, 12, 0), "- Eins\n* Zwei\n\n  Drei  ", "checkin"),
            Entry::note(at(20, 17, 0), "Vier", "checkin"),
        ];
        let week = build_week(date(20), &entries, &schedule(), &config(8.4));
        let d = &week.days[0];
        assert_eq!(d.activities, ["- Eins", "- Zwei", "- Drei", "- Vier"]);
        assert_eq!(d.times, ["4.2h", "", "", "4.2h"]);
        assert_eq!(d.total, "8.4h");
    }

    #[test]
    fn notes_without_measured_time_share_equally() {
        // beide Notizen vor Arbeitsbeginn: kein gemessenes Gewicht
        let entries = vec![
            Entry::note(at(20, 6, 0), "A", "quick"),
            Entry::note(at(20, 6, 30), "B", "quick"),
        ];
        let week = build_week(date(20), &entries, &schedule(), &config(8.4));
        assert_eq!(week.days[0].times, ["4.2h", "4.2h"]);
    }

    #[test]
    fn hours_per_day_are_capped() {
        let entries = vec![Entry::note(at(20, 17, 0), "A", "quick")];
        let week = build_week(date(20), &entries, &schedule(), &config(1e9));
        assert_eq!(week.days[0].total, "24.0h");
        let week = build_week(date(20), &entries, &schedule(), &config(f64::NAN));
        assert_eq!(week.days[0].total, "8.0h");
    }

    #[test]
    fn empty_and_special_days() {
        let mut s = schedule();
        s.school_days = vec![Weekday::Wed];
        let entries = vec![
            Entry::day(at(23, 7, 0), DayKind::Ferien),
            Entry::day(at(24, 7, 0), DayKind::Arbeit),
            Entry::day(at(24, 8, 0), DayKind::Krank),
        ];
        let week = build_week(date(22), &entries, &s, &config(8.4));
        assert_eq!(week.days.len(), 5);
        let row = |i: usize| (week.days[i].place.as_str(), week.days[i].activities[0].as_str(), week.days[i].total.as_str());
        assert_eq!(row(0), ("@Noseryoung", "–", "–")); // Montag, Arbeitstag ohne Einträge
        assert_eq!(row(1), ("@Noseryoung", "–", "–"));
        assert_eq!(row(2), ("Schule", "–", "–")); // Mittwoch, wiederkehrender Schultag
        assert_eq!(row(3), ("Ferien", "–", "–"));
        assert_eq!(row(4), ("Krank", "–", "–")); // die letzte Markierung gilt
        assert!(week.days.iter().all(|d| d.times.is_empty()));
    }

    #[test]
    fn free_weekday_and_weekend_rules() {
        let mut s = schedule();
        s.work_days = vec![Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu];
        let entries = vec![Entry::note(at(25, 10, 0), "Samstagsarbeit", "quick")];
        let week = build_week(date(20), &entries, &s, &config(8.4));
        // Mo bis Fr immer, Samstag wegen der Notiz, Sonntag nicht
        let places: Vec<_> = week.days.iter().map(|d| d.place.as_str()).collect();
        assert_eq!(places, ["@Noseryoung", "@Noseryoung", "@Noseryoung", "@Noseryoung", "Frei", "Frei"]);
        assert_eq!(week.days[5].activities, ["- Samstagsarbeit"]);
        assert_eq!(week.days[5].total, "8.4h");
    }

    #[test]
    fn school_day_notes_get_no_time() {
        let mut s = schedule();
        s.school_days = vec![Weekday::Wed];
        let entries = vec![Entry::note(at(22, 10, 0), "Modul 300", "quick")];
        let week = build_week(date(22), &entries, &s, &config(8.4));
        let wed = &week.days[2];
        assert_eq!((wed.place.as_str(), wed.total.as_str()), ("Schule", "–"));
        assert_eq!(wed.activities, ["- Modul 300"]);
        assert!(wed.times.is_empty());
    }

    #[test]
    fn commits_appear_under_their_day_without_duplicates() {
        let hash = |c: char| c.to_string().repeat(40);
        let entries = vec![
            Entry::note(at(20, 12, 0), "- Login gefixt\n- Tests geschrieben", "checkin"),
            Entry::commit(at(20, 10, 0), "recur", &hash('a'), "Login gefixt"),
            Entry::commit(at(20, 11, 0), "recur", &hash('b'), "Kalender | Grid"),
            Entry::commit(at(21, 9, 0), "recur", &hash('c'), "andere Woche? nein, Dienstag"),
        ];
        let week = build_week(date(20), &entries, &schedule(), &config(8.4));
        assert_eq!(
            week.days[0].activities,
            ["- Login gefixt", "- Tests geschrieben", "- Commit recur: Kalender | Grid"]
        );
        assert_eq!(week.days[0].times, ["8.4h", "", ""]);
        assert_eq!(week.days[1].activities, ["- Commit recur: andere Woche? nein, Dienstag"]);
        assert_eq!(week.days[1].total, "–");
    }

    #[test]
    fn last_reflection_of_the_week_wins_and_other_weeks_are_ignored() {
        let entries = vec![
            Entry::reflection(at(23, 17, 0), "alt", "alt", "alt"),
            Entry::reflection(at(24, 17, 0), "Rückblick", "Reflexion", "gut"),
        ];
        let week = build_week(date(20), &entries, &schedule(), &config(8.4));
        assert_eq!((week.review.as_str(), week.reflection.as_str(), week.mood.as_str()), ("Rückblick", "Reflexion", "gut"));
        assert_eq!((week.year, week.week), (2026, 30));
        assert_eq!((week.from, week.to), (date(20), date(26)));

        let empty = build_week(date(20), &[], &schedule(), &config(8.4));
        assert!(empty.review.is_empty());
        assert_eq!(empty.days.len(), 5);
    }

    #[test]
    fn file_name_uses_iso_week() {
        let week = build_week(date(22), &[], &schedule(), &config(8.4));
        assert_eq!(file_name(&week), "ABJ_2026-KW30.docx");
        let week = build_week(NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(), &[], &schedule(), &config(8.4));
        assert_eq!(file_name(&week), "ABJ_2026-KW53.docx");
    }

    #[test]
    fn cli_ignores_other_args() {
        assert_eq!(cli(&[]), None);
        assert_eq!(cli(&["--last".to_string()]), None);
    }
}
