use crate::docx;
use crate::journal::{Entry, Journal, Quelle, Update, WeekFile, iso_week, week_bounds};
use crate::log;
use crate::settings::{AutoTexte, DayKind, Plan, Settings};
use crate::system;
use chrono::{Datelike, Days, Local, NaiveDate, Weekday};
use std::fs;
use std::path::PathBuf;

pub const MAX_HOURS_PER_DAY: f64 = 24.0;
const NONE: &str = "–";

/// Die mitgelieferte Word-Vorlage (aus dem Beispiel-Arbeitsjournal, mit `{{Platzhaltern}}`)
const DEFAULT_TEMPLATE: &[u8] = include_bytes!("../assets/vorlage.docx");

/// Geprüfte Export-Einstellungen
#[derive(Debug, Clone, PartialEq)]
pub struct ExportConfig {
    pub nachname: String,
    pub vorname: String,
    /// `None`: Standardordner `Dokumente\Notify`
    pub dir: Option<PathBuf>,
    /// `None`: die mitgelieferte Vorlage
    pub vorlage: Option<PathBuf>,
    pub rest_auffuellen: bool,
    pub plan: Plan,
    pub ferien: AutoTexte,
    pub uek: AutoTexte,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DayData {
    pub date: NaiveDate,
    /// Zum Beispiel `@Noser Young`, ohne Ort `Frei`
    pub place: String,
    pub kind: Option<DayKind>,
    pub activities: Vec<String>,
    /// Gleich lang wie `activities` (leere Texte bei Zeilen ohne Zeit)
    pub times: Vec<String>,
    /// Tagestotal wie `8.4h`, leer bei Ferien und üK, `–` ohne Stunden
    pub total: String,
    pub total_tenths: i64,
    pub soll_tenths: i64,
}

impl DayData {
    /// Abweichung vom Tagessoll in Zehntelstunden (negativ: es fehlen Stunden)
    #[cfg(test)]
    pub fn abweichung(&self) -> i64 {
        self.total_tenths - self.soll_tenths
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WeekData {
    pub year: i32,
    pub week: u32,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub nachname: String,
    pub vorname: String,
    pub days: Vec<DayData>,
    pub review: String,
    pub reflection: String,
    pub mood: String,
    /// `Ferien` oder `Uek`, wenn die ganze Woche automatisch befüllt wurde
    pub auto: Option<DayKind>,
}

impl WeekData {
    /// Freitag der Woche: das Datum der Unterschrift
    pub fn friday(&self) -> NaiveDate {
        self.from.checked_add_days(Days::new(4)).unwrap_or(self.from)
    }
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
    let sign = if tenths < 0 { "-" } else { "" };
    let tenths = tenths.abs();
    format!("{sign}{}.{}h", tenths / 10, tenths % 10)
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

/// Zeilen eines Tages in Zehntelstunden, bei denen der fehlende Rest bis `soll` auf die letzte Zeile kommt.
/// Ist die Summe schon gleich oder grösser als das Soll, bleibt alles wie es ist.
pub fn rest_auf_letzte_zeile(tenths: &[i64], soll: i64) -> Vec<i64> {
    let mut out = tenths.to_vec();
    let rest = soll - tenths.iter().sum::<i64>();
    if out.is_empty() || rest <= 0 {
        return out;
    }
    // Nur die letzte Zeile hat ein Gewicht, also bekommt sie den ganzen Rest
    let mut weights = vec![0; out.len()];
    weights[out.len() - 1] = 1;
    for (value, add) in out.iter_mut().zip(distribute(rest, &weights)) {
        *value += add;
    }
    out
}

/// Zählt im Export und in der Tagessumme: manuelle Einträge, Commits erst mit Dauer
pub fn counts(entry: &Entry) -> bool {
    entry.quelle == Quelle::Manuell || entry.stunden > 0.0
}

/// Summe der Dauer eines Tages in Zehntelstunden
pub fn day_total_tenths(entries: &[Entry]) -> i64 {
    entries.iter().filter(|e| counts(e)).map(Entry::tenths).sum()
}

/// Abweichung der Tagessumme vom Soll in Zehntelstunden (negativ: es fehlen Stunden)
#[cfg(test)]
pub fn deviation_tenths(entries: &[Entry], soll: i64) -> i64 {
    day_total_tenths(entries) - soll
}

/// Schreibt den fehlenden Rest des Tages `date` auf die letzte Zeile (Knopf "Rest auf letzte Zeile").
/// Gibt den geänderten Eintrag zurück, oder `None`, wenn es nichts zu tun gibt.
pub fn rest_buchen(journal: &Journal, date: NaiveDate, soll: i64) -> Result<Option<Entry>, String> {
    let entries: Vec<Entry> = journal.day(date).into_iter().filter(counts).collect();
    let tenths: Vec<i64> = entries.iter().map(Entry::tenths).collect();
    let filled = rest_auf_letzte_zeile(&tenths, soll);
    let Some(last) = entries.last() else { return Ok(None) };
    let new = filled[filled.len() - 1];
    if new == last.tenths() {
        return Ok(None);
    }
    journal
        .update(&last.id, Update { text: last.text.clone(), stunden: new as f64 / 10.0 })
        .map(Some)
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

fn is_weekend(date: NaiveDate) -> bool {
    matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
}

fn build_day(date: NaiveDate, file: &WeekFile, config: &ExportConfig, auto: Option<(DayKind, &AutoTexte)>) -> Option<DayData> {
    let ort = config.plan.resolve(date, file.override_of(date));
    let kind = ort.map(|o| o.art);
    let mut day: Vec<&Entry> = file.eintraege.iter().filter(|e| e.local_date() == date && counts(e)).collect();
    day.sort_by_key(|e| e.t);
    if is_weekend(date) && ort.is_none() && day.is_empty() {
        return None;
    }

    let soll = ort.map_or(0, |o| o.soll_tenths());
    // Ferien und üK haben keine Stunden, bei allen anderen Tagen füllt der Schalter den Rest auf
    let mut tenths: Vec<i64> = day.iter().map(|e| e.tenths()).collect();
    if config.rest_auffuellen && matches!(kind, Some(DayKind::Arbeit | DayKind::Schule)) {
        tenths = rest_auf_letzte_zeile(&tenths, soll);
    }

    let mut activities = Vec::new();
    let mut times = Vec::new();
    for (entry, &t) in day.iter().zip(&tenths) {
        let text = if entry.quelle == Quelle::Commit {
            format!("Commit {}: {}", entry.repo.as_deref().unwrap_or("?"), entry.text)
        } else {
            entry.text.clone()
        };
        let lines: Vec<&str> = text.lines().map(strip_bullet).filter(|l| !l.is_empty()).collect();
        for (n, line) in lines.iter().enumerate() {
            activities.push(format!("- {line}"));
            times.push(if n == 0 && t > 0 { format_hours(t) } else { String::new() });
        }
    }
    // Reine Ferien-/üK-Woche: Tage, an denen der Nutzer nichts geschrieben hat, bekommen den festen Satz
    if activities.is_empty()
        && let Some((_, texts)) = auto
        && !texts.taetigkeit.trim().is_empty()
    {
        activities.push(format!("- {}", texts.taetigkeit.trim()));
        times.push(String::new());
    }
    if activities.is_empty() {
        activities.push(NONE.to_string());
        times.push(String::new());
    }

    let total_tenths: i64 = tenths.iter().sum();
    let total = if total_tenths > 0 {
        format_hours(total_tenths)
    } else if matches!(kind, Some(DayKind::Ferien | DayKind::Uek)) {
        String::new()
    } else {
        NONE.to_string()
    };
    let place = ort.map_or_else(|| "Frei".to_string(), |o| format!("@{}", o.name.trim().trim_start_matches('@')));

    Some(DayData { date, place, kind, activities, times, total, total_tenths, soll_tenths: soll })
}

/// `Uek`, wenn alle fünf Arbeitstage Ferien oder üK sind und mindestens einer üK ist; `Ferien`, wenn alle Ferien sind
fn auto_kind(kinds: &[Option<DayKind>]) -> Option<DayKind> {
    if kinds.len() < 5 || !kinds.iter().all(|k| matches!(k, Some(DayKind::Ferien | DayKind::Uek))) {
        return None;
    }
    if kinds.contains(&Some(DayKind::Uek)) { Some(DayKind::Uek) } else { Some(DayKind::Ferien) }
}

/// Baut die Daten der Woche, in der `date` liegt. Stunden kommen nur aus den Einträgen
/// (bei `rest_auffuellen` zusätzlich der Rest bis zum Tagessoll), nichts wird geschätzt.
pub fn build_week(date: NaiveDate, file: &WeekFile, config: &ExportConfig) -> WeekData {
    let (monday, sunday) = week_bounds(date);
    let (year, week) = iso_week(monday);

    let kinds: Vec<Option<DayKind>> = (0..5u64)
        .map(|offset| {
            let day = monday.checked_add_days(Days::new(offset)).unwrap_or(monday);
            config.plan.resolve(day, file.override_of(day)).map(|o| o.art)
        })
        .collect();
    let auto = auto_kind(&kinds);
    let auto_texts = auto.map(|kind| {
        let texts = if kind == DayKind::Uek { &config.uek } else { &config.ferien };
        (kind, texts)
    });

    let days = (0..7u64)
        .filter_map(|offset| monday.checked_add_days(Days::new(offset)))
        .filter_map(|day| build_day(day, file, config, auto_texts))
        .collect();

    // Selbst geschriebene Wochentexte gehen vor den festen Texten
    let pick = |own: &str, auto_text: fn(&AutoTexte) -> &String| -> String {
        if !own.trim().is_empty() {
            return own.to_string();
        }
        auto_texts.map_or_else(String::new, |(_, texts)| auto_text(texts).trim().to_string())
    };
    WeekData {
        year,
        week,
        from: monday,
        to: sunday,
        nachname: config.nachname.clone(),
        vorname: config.vorname.clone(),
        days,
        review: pick(&file.texte.rueckblick, |t| &t.rueckblick),
        reflection: pick(&file.texte.reflexion, |t| &t.reflexion),
        mood: pick(&file.texte.stimmung, |t| &t.stimmung),
        auto,
    }
}

fn default_dir() -> PathBuf {
    system::documents_dir().unwrap_or_else(std::env::temp_dir).join("Notify")
}

/// Name in Kleinbuchstaben, ohne Zeichen, die Windows in Dateinamen verbietet; Leerraum wird zu `-`
fn slug(name: &str) -> String {
    let clean: String = name
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') && !c.is_control())
        .collect();
    clean.split_whitespace().collect::<Vec<_>>().join("-")
}

/// `Arbeitsjournal-<nachname>-<vorname>-<jahr>-<KW>.docx`
pub fn file_name(week: &WeekData) -> String {
    format!("Arbeitsjournal-{}-{}-{}-{:02}.docx", slug(&week.nachname), slug(&week.vorname), week.year, week.week)
}

/// Exportiert die Woche, in der `date` liegt, und gibt den Pfad der Datei zurück
pub fn export_week_of(date: NaiveDate) -> Result<PathBuf, String> {
    let settings = Settings::load();
    let config = settings.export()?;
    if config.nachname.is_empty() || config.vorname.is_empty() {
        return Err("Nachname und Vorname fehlen (Einstellungen), sie stehen im Dateinamen und in der Kopfzeile".to_string());
    }

    let journal = Journal::from_settings(&settings);
    let week = build_week(date, &journal.week_file(date), &config);

    let template = match &config.vorlage {
        Some(path) => fs::read(path).map_err(|e| format!("Word-Vorlage {} nicht lesbar: {e}", path.display()))?,
        None => DEFAULT_TEMPLATE.to_vec(),
    };
    let bytes = docx::fill_template(&template, &week)?;

    let dir = config.dir.unwrap_or_else(default_dir);
    fs::create_dir_all(&dir).map_err(|e| format!("Exportordner {} nicht erstellbar: {e}", dir.display()))?;
    let path = dir.join(file_name(&week));
    fs::write(&path, bytes).map_err(|e| {
        format!("{} nicht schreibbar (ist die Datei in Word geöffnet?): {e}", path.display())
    })?;
    log::info(&format!("Export geschrieben: {}", path.display()));
    Ok(path)
}

/// Exportiert die aktuelle (`last == false`) oder die vorige Woche
pub fn export_week(today: NaiveDate, last: bool) -> Result<PathBuf, String> {
    let date = if last { today.checked_sub_days(Days::new(7)).unwrap_or(today) } else { today };
    export_week_of(date)
}

/// Die mitgelieferte Vorlage (für Tests)
#[cfg(test)]
pub fn default_template() -> &'static [u8] {
    DEFAULT_TEMPLATE
}

/// Speichert die mitgelieferte Vorlage als Ausgangspunkt für eine eigene Vorlage in den Exportordner
pub fn save_default_template() -> Result<PathBuf, String> {
    let config = Settings::load().export()?;
    let dir = config.dir.unwrap_or_else(default_dir);
    fs::create_dir_all(&dir).map_err(|e| format!("Exportordner {} nicht erstellbar: {e}", dir.display()))?;
    let path = dir.join("Vorlage-Arbeitsjournal.docx");
    fs::write(&path, DEFAULT_TEMPLATE).map_err(|e| format!("{} nicht schreibbar: {e}", path.display()))?;
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
pub mod tests {
    use super::*;
    use crate::journal::{DayInfo, WeekTexts};
    use crate::settings::{PlanTag, Settings};
    use chrono::{DateTime, TimeZone, Utc};

    pub fn at(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        // Juli 2026: 20 = Montag
        Local.with_ymd_and_hms(2026, 7, d, h, m, 0).unwrap().with_timezone(&Utc)
    }

    pub fn date(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, d).unwrap()
    }

    /// Mo bis Fr alle `Noser Young`, wie im Beispieldokument KW30
    pub fn config() -> ExportConfig {
        let tags = [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri];
        Settings {
            wochenplan: tags.iter().map(|&tag| PlanTag { tag, ort: "Noser Young".into() }).collect(),
            nachname: "Maurer".into(),
            vorname: "Jemuel".into(),
            ..Settings::default()
        }
        .export()
        .unwrap()
    }

    pub fn week_file(entries: Vec<Entry>) -> WeekFile {
        WeekFile { jahr: 2026, kw: 30, eintraege: entries, ..WeekFile::default() }
    }

    pub fn override_day(file: &mut WeekFile, d: u32, ort: &str) {
        file.tage.insert(format!("2026-07-{d:02}"), DayInfo { ort: ort.into(), abweichend: true });
    }

    fn entry(d: u32, h: u32, text: &str, stunden: f64) -> Entry {
        Entry::manuell(at(d, h, 0), text, stunden)
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
    fn rest_goes_to_the_last_line_only() {
        assert_eq!(rest_auf_letzte_zeile(&[20, 30], 84), vec![20, 64]);
        assert_eq!(rest_auf_letzte_zeile(&[24, 0, 10], 84), vec![24, 0, 60]);
        assert_eq!(rest_auf_letzte_zeile(&[0], 84), vec![84]);
        // schon erfüllt oder überschritten: nichts ändern
        assert_eq!(rest_auf_letzte_zeile(&[44, 40], 84), vec![44, 40]);
        assert_eq!(rest_auf_letzte_zeile(&[60, 40], 84), vec![60, 40]);
        // ohne Zeile gibt es keinen Ort für den Rest
        assert!(rest_auf_letzte_zeile(&[], 84).is_empty());
        assert_eq!(rest_auf_letzte_zeile(&[10], 0), vec![10]);
    }

    #[test]
    fn day_sum_and_deviation() {
        let hash = "a".repeat(40);
        let entries = vec![
            entry(20, 10, "A", 2.4),
            entry(20, 11, "B", 1.1),
            // ein Commit ohne Dauer zählt nicht, mit Dauer schon
            Entry::commit(at(20, 12, 0), "r", &hash, "msg"),
            Entry { stunden: 0.5, ..Entry::commit(at(20, 13, 0), "r", &hash, "msg2") },
        ];
        assert_eq!(day_total_tenths(&entries), 40);
        assert_eq!(deviation_tenths(&entries, 84), -44);
        assert_eq!(deviation_tenths(&entries, 40), 0);
        assert_eq!(deviation_tenths(&entries, 30), 10);
        assert_eq!(day_total_tenths(&[]), 0);
    }

    #[test]
    fn rest_buchen_updates_the_last_line_in_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::new(dir.path().join("weeks"));
        assert_eq!(rest_buchen(&j, date(20), 84).unwrap(), None);

        j.add(entry(20, 10, "A", 2.4)).unwrap();
        let last = j.add(entry(20, 12, "B", 1.0)).unwrap();
        let done = rest_buchen(&j, date(20), 84).unwrap().unwrap();
        assert_eq!((done.id.as_str(), done.stunden), (last.id.as_str(), 6.0));
        assert_eq!(day_total_tenths(&j.day(date(20))), 84);
        // ein zweiter Aufruf hat nichts mehr zu tun
        assert_eq!(rest_buchen(&j, date(20), 84).unwrap(), None);
    }

    #[test]
    fn rest_buchen_does_nothing_when_the_day_is_full() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::new(dir.path().join("weeks"));
        j.add(entry(20, 10, "A", 8.4)).unwrap();
        assert_eq!(rest_buchen(&j, date(20), 84).unwrap(), None);
        assert_eq!(j.day(date(20))[0].stunden, 8.4);
    }

    #[test]
    fn formats_hours_and_weekdays() {
        assert_eq!(format_hours(84), "8.4h");
        assert_eq!(format_hours(30), "3.0h");
        assert_eq!(format_hours(0), "0.0h");
        assert_eq!(format_hours(105), "10.5h");
        assert_eq!(format_hours(-4), "-0.4h");
        assert_eq!(german_weekday(date(20)), "Montag");
        assert_eq!(german_weekday(date(26)), "Sonntag");
    }

    #[test]
    fn hours_come_only_from_the_entries() {
        let file = week_file(vec![
            entry(20, 11, "Erste", 3.0),
            entry(20, 14, "Zweite", 2.0),
            entry(20, 15, "Dritte", 1.0),
            entry(20, 17, "Vierte", 2.4),
        ]);
        let week = build_week(date(20), &file, &config());
        let monday = &week.days[0];
        assert_eq!(monday.place, "@Noser Young");
        assert_eq!(monday.activities, ["- Erste", "- Zweite", "- Dritte", "- Vierte"]);
        assert_eq!(monday.times, ["3.0h", "2.0h", "1.0h", "2.4h"]);
        assert_eq!(monday.total, "8.4h");
        assert_eq!((monday.total_tenths, monday.soll_tenths, monday.abweichung()), (84, 84, 0));
    }

    #[test]
    fn deviation_is_available_per_day() {
        let file = week_file(vec![entry(20, 11, "A", 6.4), entry(21, 11, "B", 9.0)]);
        let week = build_week(date(20), &file, &config());
        assert_eq!(week.days[0].abweichung(), -20);
        assert_eq!(week.days[1].abweichung(), 6);
        // Tag ohne Einträge: es fehlt das ganze Soll
        assert_eq!(week.days[2].abweichung(), -84);
        assert_eq!(week.days[2].total, "–");
    }

    #[test]
    fn rest_is_only_filled_when_enabled() {
        let file = week_file(vec![entry(20, 11, "A", 2.0), entry(20, 14, "B", 1.0)]);
        let off = build_week(date(20), &file, &config());
        assert_eq!(off.days[0].times, ["2.0h", "1.0h"]);
        assert_eq!(off.days[0].total, "3.0h");

        let on = ExportConfig { rest_auffuellen: true, ..config() };
        let week = build_week(date(20), &file, &on);
        assert_eq!(week.days[0].times, ["2.0h", "6.4h"]);
        assert_eq!(week.days[0].total, "8.4h");
        // Tage ohne Zeilen bleiben leer, es gibt keine Zeile für den Rest
        assert_eq!(week.days[1].total, "–");
    }

    #[test]
    fn multi_line_entries_get_one_time_on_the_first_line() {
        let file = week_file(vec![entry(20, 12, "- Eins\n* Zwei\n\n  Drei  ", 4.2), entry(20, 17, "Vier", 4.2)]);
        let week = build_week(date(20), &file, &config());
        let d = &week.days[0];
        assert_eq!(d.activities, ["- Eins", "- Zwei", "- Drei", "- Vier"]);
        assert_eq!(d.times, ["4.2h", "", "", "4.2h"]);
        assert_eq!(d.total, "8.4h");
    }

    #[test]
    fn entries_without_hours_show_no_time() {
        let file = week_file(vec![entry(20, 11, "A", 0.0), entry(20, 12, "B", 1.5)]);
        let d = &build_week(date(20), &file, &config()).days[0];
        assert_eq!(d.times, ["", "1.5h"]);
        assert_eq!(d.total, "1.5h");
        let file = week_file(vec![entry(20, 11, "A", 0.0)]);
        let d = &build_week(date(20), &file, &config()).days[0];
        assert_eq!(d.times, [""]);
        assert_eq!(d.total, "–");
    }

    #[test]
    fn places_follow_override_then_plan() {
        let mut file = week_file(vec![]);
        override_day(&mut file, 22, "Gibb");
        override_day(&mut file, 23, "@Ferien");
        let week = build_week(date(20), &file, &config());
        let places: Vec<_> = week.days.iter().map(|d| d.place.as_str()).collect();
        assert_eq!(places, ["@Noser Young", "@Noser Young", "@Gibb", "@Ferien", "@Noser Young"]);
        assert_eq!(week.days[2].kind, Some(DayKind::Schule));
        assert_eq!(week.days[3].total, "");
        assert_eq!(week.auto, None);

        // Standardplan ohne Overrides: Mo-Mi Noser Young, Do-Fr Gibb
        let default = Settings::default().export().unwrap();
        let week = build_week(date(20), &week_file(vec![]), &default);
        let places: Vec<_> = week.days.iter().map(|d| d.place.as_str()).collect();
        assert_eq!(places, ["@Noser Young", "@Noser Young", "@Noser Young", "@Gibb", "@Gibb"]);
    }

    #[test]
    fn weekend_rules() {
        let c = config();
        let week = build_week(date(20), &week_file(vec![]), &c);
        assert_eq!(week.days.len(), 5);
        // Samstagsarbeit: der Tag erscheint, ohne Ort als "Frei"
        let week = build_week(date(20), &week_file(vec![entry(25, 10, "Samstagsarbeit", 2.0)]), &c);
        assert_eq!(week.days.len(), 6);
        assert_eq!((week.days[5].place.as_str(), week.days[5].total.as_str()), ("Frei", "2.0h"));
        assert_eq!(week.days[5].soll_tenths, 0);
        // Override am Sonntag macht ihn sichtbar
        let mut file = week_file(vec![]);
        override_day(&mut file, 26, "Noser Young");
        assert_eq!(build_week(date(20), &file, &c).days.len(), 6);
    }

    #[test]
    fn commits_need_hours_to_appear() {
        let hash = "a".repeat(40);
        let file = week_file(vec![
            entry(20, 12, "Eigene Zeile", 1.0),
            Entry::commit(at(20, 10, 0), "recur", &hash, "nur Hinweis"),
            Entry { stunden: 0.5, ..Entry::commit(at(20, 11, 0), "recur", &hash, "Kalender | Grid") },
        ]);
        let week = build_week(date(20), &file, &config());
        assert_eq!(week.days[0].activities, ["- Commit recur: Kalender | Grid", "- Eigene Zeile"]);
        assert_eq!(week.days[0].times, ["0.5h", "1.0h"]);
        assert_eq!(week.days[0].total, "1.5h");
    }

    #[test]
    fn user_week_texts_and_other_weeks() {
        let mut file = week_file(vec![entry(20, 10, "x", 1.0)]);
        file.texte = WeekTexts { rueckblick: "Rückblick".into(), reflexion: "Reflexion".into(), stimmung: "gut".into() };
        let week = build_week(date(22), &file, &config());
        assert_eq!((week.review.as_str(), week.reflection.as_str(), week.mood.as_str()), ("Rückblick", "Reflexion", "gut"));
        assert_eq!((week.year, week.week), (2026, 30));
        assert_eq!((week.from, week.to), (date(20), date(26)));
        assert_eq!((week.nachname.as_str(), week.vorname.as_str()), ("Maurer", "Jemuel"));

        let empty = build_week(date(20), &week_file(vec![]), &config());
        assert!(empty.review.is_empty() && empty.reflection.is_empty() && empty.mood.is_empty());
        assert_eq!(empty.days.len(), 5);
    }

    fn all_days(file: &mut WeekFile, ort: &str) {
        for d in 20..=24 {
            override_day(file, d, ort);
        }
    }

    #[test]
    fn pure_holiday_week_gets_the_fixed_texts() {
        let mut file = week_file(vec![]);
        all_days(&mut file, "Ferien");
        let week = build_week(date(20), &file, &config());
        assert_eq!(week.auto, Some(DayKind::Ferien));
        for d in &week.days {
            assert_eq!(d.activities, ["- Ich habe die Ferien genossen"]);
            assert_eq!(d.total, "");
            assert_eq!(d.times, [""]);
            assert_eq!(d.place, "@Ferien");
        }
        assert_eq!(week.review, "Es gibt keinen Wochenrückblick, da ich in den Ferien war.");
        assert_eq!(week.reflection, "Es gibt keine Reflexion, da ich in den Ferien war.");
        assert_eq!(week.mood, "Es gibt keine Stimmung der Woche, da ich in den Ferien war.");
    }

    #[test]
    fn pure_uek_week_gets_the_fixed_texts() {
        let mut file = week_file(vec![]);
        all_days(&mut file, "üK");
        let week = build_week(date(20), &file, &config());
        assert_eq!(week.auto, Some(DayKind::Uek));
        assert!(week.days.iter().all(|d| d.activities == ["- Wir hatten üK"] && d.total.is_empty()));
        assert_eq!(week.review, "Diese Woche gibt es keinen Wochenrückblick, da wir einen üK hatten.");
        assert_eq!(week.reflection, "Diese Woche gibt es keine Wochenreflexion, da wir einen üK hatten.");
        assert_eq!(week.mood, "Diese Woche gibt es keine Stimmung der Woche, da wir einen üK hatten.");
    }

    #[test]
    fn uek_wins_over_holidays_when_all_five_days_are_either() {
        let mut file = week_file(vec![]);
        all_days(&mut file, "Ferien");
        override_day(&mut file, 22, "üK");
        let week = build_week(date(20), &file, &config());
        assert_eq!(week.auto, Some(DayKind::Uek));
        // alle fünf Tage bekommen den üK-Satz, auch die Ferientage
        assert!(week.days.iter().all(|d| d.activities == ["- Wir hatten üK"]));
        // ... aber die Orte bleiben, wie sie sind
        assert_eq!(week.days[0].place, "@Ferien");
        assert_eq!(week.days[2].place, "@üK");
    }

    #[test]
    fn mixed_weeks_get_nothing_automatic() {
        let mut file = week_file(vec![]);
        all_days(&mut file, "Ferien");
        override_day(&mut file, 24, "Noser Young");
        let week = build_week(date(20), &file, &config());
        assert_eq!(week.auto, None);
        assert!(week.days.iter().all(|d| d.activities == ["–"]));
        assert!(week.review.is_empty() && week.reflection.is_empty() && week.mood.is_empty());
        // Ferien-/üK-Tage haben trotzdem kein Tagestotal
        assert_eq!(week.days[0].total, "");
        assert_eq!(week.days[4].total, "–");

        // vier Ferientage und ein Gibb-Tag: auch gemischt
        let mut file = week_file(vec![]);
        all_days(&mut file, "Ferien");
        override_day(&mut file, 21, "Gibb");
        assert_eq!(build_week(date(20), &file, &config()).auto, None);
    }

    #[test]
    fn own_text_beats_the_fixed_text() {
        let mut file = week_file(vec![entry(21, 10, "Doch etwas gearbeitet", 2.0)]);
        all_days(&mut file, "Ferien");
        file.texte.stimmung = "Erholt".into();
        let week = build_week(date(20), &file, &config());
        assert_eq!(week.auto, Some(DayKind::Ferien));
        assert_eq!(week.days[0].activities, ["- Ich habe die Ferien genossen"]);
        assert_eq!(week.days[1].activities, ["- Doch etwas gearbeitet"]);
        assert_eq!(week.days[1].times, ["2.0h"]);
        assert_eq!(week.days[1].total, "2.0h");
        assert_eq!(week.mood, "Erholt");
        assert_eq!(week.review, "Es gibt keinen Wochenrückblick, da ich in den Ferien war.");
    }

    #[test]
    fn fixed_texts_come_from_the_settings() {
        let mut c = config();
        c.ferien.taetigkeit = "Erholung".into();
        c.ferien.rueckblick = "Frei".into();
        let mut file = week_file(vec![]);
        all_days(&mut file, "Ferien");
        let week = build_week(date(20), &file, &c);
        assert_eq!(week.days[0].activities, ["- Erholung"]);
        assert_eq!(week.review, "Frei");
    }

    #[test]
    fn file_name_and_friday() {
        let week = build_week(date(22), &week_file(vec![]), &config());
        assert_eq!(file_name(&week), "Arbeitsjournal-maurer-jemuel-2026-30.docx");
        assert_eq!(week.friday(), date(24));
        assert_eq!(week.friday().format("%d.%m.%y").to_string(), "24.07.26");

        // Jahreswechsel: der 1.1.2027 liegt in KW53 von 2026, der Freitag ist der 1.1.2027
        let jan = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();
        let week = build_week(jan, &WeekFile::default(), &config());
        assert_eq!(file_name(&week), "Arbeitsjournal-maurer-jemuel-2026-53.docx");
        assert_eq!(week.friday(), jan);

        // Namen: Kleinbuchstaben, Leerraum zu Bindestrich, verbotene Zeichen weg
        let mut c = config();
        c.nachname = " Von  Arx ".into();
        c.vorname = "Jo/hn:".into();
        let week = build_week(date(20), &week_file(vec![]), &c);
        assert_eq!(file_name(&week), "Arbeitsjournal-von-arx-john-2026-30.docx");
    }

    #[test]
    fn single_digit_weeks_are_padded() {
        let week = build_week(NaiveDate::from_ymd_opt(2026, 1, 28).unwrap(), &WeekFile::default(), &config());
        assert_eq!(file_name(&week), "Arbeitsjournal-maurer-jemuel-2026-05.docx");
    }

    #[test]
    fn cli_ignores_other_args() {
        assert_eq!(cli(&[]), None);
        assert_eq!(cli(&["--last".to_string()]), None);
    }
}
