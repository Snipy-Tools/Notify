use crate::export::{ExportConfig, build_week, counts, day_total_tenths, file_name, german_weekday};
use crate::journal::{Entry, MAX_HOURS, Quelle, WeekFile, iso_week, round_hours, split_duration, week_bounds};
use crate::settings::{DayKind, Plan};
use chrono::{DateTime, Days, Local, NaiveDate, TimeZone, Utc};
use serde_json::{Value, json};

pub fn label(date: NaiveDate) -> String {
    format!("{} {}", german_weekday(date), date.format("%d.%m."))
}

fn short_weekday(date: NaiveDate) -> &'static str {
    &german_weekday(date)[..2]
}

/// Verschiebt den Tag um `delta`, aber nie über `today` hinaus
pub fn shift(date: NaiveDate, delta: i64, today: NaiveDate) -> NaiveDate {
    let moved = if delta >= 0 {
        date.checked_add_days(Days::new(delta.unsigned_abs()))
    } else {
        date.checked_sub_days(Days::new(delta.unsigned_abs()))
    };
    moved.unwrap_or(date).min(today)
}

/// Verschiebt um ganze Wochen (auch in die Zukunft, z. B. um Ferien vorzumerken)
pub fn shift_weeks(date: NaiveDate, delta: i64) -> NaiveDate {
    let days = delta.saturating_mul(7);
    let moved = if days >= 0 {
        date.checked_add_days(Days::new(days.unsigned_abs()))
    } else {
        date.checked_sub_days(Days::new(days.unsigned_abs()))
    };
    moved.unwrap_or(date)
}

/// Zehntelstunden als "6.4"
pub fn fmt_tenths(tenths: i64) -> String {
    let sign = if tenths < 0 { "-" } else { "" };
    format!("{sign}{}.{}", tenths.abs() / 10, tenths.abs() % 10)
}

/// Tagessumme gegen das Soll als neutraler Text
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// "6.4 / 8.4 h"
    pub main: String,
    /// "noch 2.0 h offen", "0.6 h über dem Soll" oder leer
    pub note: String,
    /// "none" (kein Soll), "open", "done" oder "over"
    pub level: &'static str,
}

pub fn summary(total: i64, soll: i64) -> Summary {
    if soll <= 0 {
        return Summary { main: format!("{} h", fmt_tenths(total)), note: String::new(), level: "none" };
    }
    let main = format!("{} / {} h", fmt_tenths(total), fmt_tenths(soll));
    match total.cmp(&soll) {
        std::cmp::Ordering::Less => {
            Summary { main, note: format!("noch {} h offen", fmt_tenths(soll - total)), level: "open" }
        }
        std::cmp::Ordering::Equal => Summary { main, note: "Soll erreicht".to_string(), level: "done" },
        std::cmp::Ordering::Greater => {
            Summary { main, note: format!("{} h über dem Soll", fmt_tenths(total - soll)), level: "over" }
        }
    }
}

/// Text und Dauer aus der Eingabezeile. Ein eigenes Dauerfeld gilt streng (Fehler statt Raten),
/// sonst wird eine Dauer am Ende des Textes abgetrennt (`Bug gefixt 2.4`).
pub fn parse_entry_input(text: &str, hours: &str) -> Result<(String, f64), String> {
    let hours = hours.trim();
    if hours.is_empty() {
        return Ok(split_duration(text));
    }
    let number = hours.strip_suffix(['h', 'H']).unwrap_or(hours).trim().replace(',', ".");
    let valid = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit() || c == '.');
    match number.parse::<f64>() {
        Ok(h) if valid && h <= MAX_HOURS => Ok((text.trim().to_string(), round_hours(h))),
        _ => Err(format!("Die Dauer \"{hours}\" ist ungültig (erlaubt: 0 bis {MAX_HOURS} Stunden, z. B. 2.4)")),
    }
}

/// Zeitpunkt für einen neuen Eintrag am Tag `date`: heute jetzt, sonst dieselbe Uhrzeit am gewählten Tag
pub fn entry_time(date: NaiveDate, now: DateTime<Local>) -> DateTime<Utc> {
    if date == now.date_naive() {
        return now.with_timezone(&Utc);
    }
    Local
        .from_local_datetime(&date.and_time(now.time()))
        .earliest()
        .map_or_else(|| now.with_timezone(&Utc), |t| t.with_timezone(&Utc))
}

/// Text zum Bearbeiten: der Text, gefolgt von der Dauer
pub fn edit_text(entry: &Entry) -> String {
    if entry.stunden > 0.0 { format!("{} {}", entry.text, entry.stunden) } else { entry.text.clone() }
}

fn entry_view(entry: &Entry) -> Value {
    json!({
        "id": entry.id,
        "commit": entry.quelle == Quelle::Commit,
        "time": entry.t.with_timezone(&Local).format("%H:%M").to_string(),
        "text": entry.text,
        "repo": entry.repo,
        "hours": entry.stunden,
        "edit": edit_text(entry),
    })
}

fn ort_names(plan: &Plan) -> Vec<&str> {
    plan.orte.iter().map(|o| o.name.as_str()).collect()
}

/// Name der Art für die Oberfläche (Farbe und Icon des Ortes hängen daran)
pub fn art_name(art: Option<DayKind>) -> &'static str {
    match art {
        Some(DayKind::Arbeit) => "arbeit",
        Some(DayKind::Schule) => "schule",
        Some(DayKind::Uek) => "uek",
        Some(DayKind::Ferien) => "ferien",
        None => "",
    }
}

/// Orte mit Art für die Segmentleiste und die Auswahl
fn ort_list(plan: &Plan) -> Vec<Value> {
    plan.orte.iter().map(|o| json!({ "name": o.name, "art": art_name(Some(o.art)) })).collect()
}

/// Kurztext neben der Summe: "noch 2.0 h", "Soll erreicht", "+0.8 h zu viel"
pub fn short_note(total: i64, soll: i64) -> String {
    if soll <= 0 {
        return String::new();
    }
    match total.cmp(&soll) {
        std::cmp::Ordering::Less => format!("noch {} h", fmt_tenths(soll - total)),
        std::cmp::Ordering::Equal => "Soll erreicht".to_string(),
        std::cmp::Ordering::Greater => format!("+{} h zu viel", fmt_tenths(total - soll)),
    }
}

/// Füllgrad des Balkens in Prozent (0 bis 100)
pub fn percent(total: i64, soll: i64) -> i64 {
    if soll <= 0 { 0 } else { (total.max(0) * 100 / soll).clamp(0, 100) }
}

/// Zeigt die Pille statt Stunden den Hinweis "Keine Erinnerungen heute"? Bei üK und Ferien (keine Stunden),
/// ohne Ort oder Soll, und an einem Gibb-Tag, an dem nichts eingetragen ist.
pub fn no_hours(art: Option<DayKind>, soll: i64, total: i64) -> bool {
    match art {
        None | Some(DayKind::Uek | DayKind::Ferien) => true,
        Some(DayKind::Schule) => soll <= 0 || total == 0,
        Some(DayKind::Arbeit) => soll <= 0,
    }
}

/// Daten der Leiste für den Tag `date`. `entries` sind die Einträge des Tages, `ort_override` der Override der Woche.
pub fn bar_view(
    date: NaiveDate,
    today: NaiveDate,
    entries: &[Entry],
    ort_override: Option<&str>,
    plan: &Plan,
    collapsed: bool,
) -> Value {
    let ort = plan.resolve(date, ort_override);
    let soll = ort.map_or(0, |o| o.soll_tenths());
    let total = day_total_tenths(entries);
    let sum = summary(total, soll);
    let can_rest = soll > total && entries.iter().any(counts);
    let art = ort.map(|o| o.art);
    json!({
        "date": date.format("%Y-%m-%d").to_string(),
        "label": label(date),
        "title": format!("{}, {}", german_weekday(date), date.format("%d.%m.")),
        "short": format!("{} {}", short_weekday(date), date.format("%d.%m.")),
        "kw": iso_week(date).1,
        "is_today": date == today,
        "can_next": date < today,
        "ort": ort.map_or("", |o| o.name.as_str()),
        "orte": ort_names(plan),
        "ort_list": ort_list(plan),
        "art": art_name(art),
        "soll": fmt_tenths(soll),
        "total": fmt_tenths(total),
        "short_note": short_note(total, soll),
        "percent": percent(total, soll),
        "no_hours": no_hours(art, soll, total),
        "entries": entries.iter().map(entry_view).collect::<Vec<_>>(),
        "main": sum.main,
        "note": sum.note,
        "level": sum.level,
        "can_rest": can_rest,
        "collapsed": collapsed,
    })
}

/// Daten der Wochenansicht für die Woche, in der `date` liegt
pub fn week_view(date: NaiveDate, today: NaiveDate, file: &WeekFile, config: &ExportConfig) -> Value {
    let (monday, sunday) = week_bounds(date);
    let (year, kw) = iso_week(monday);
    let plan = &config.plan;

    let mut overrides = 0;
    let days: Vec<Value> = (0..5u64)
        .filter_map(|n| monday.checked_add_days(Days::new(n)))
        .map(|day| {
            let over = file.override_of(day);
            overrides += usize::from(over.is_some());
            let ort = plan.resolve(day, over);
            let entries: Vec<Entry> = file.eintraege.iter().filter(|e| e.local_date() == day).cloned().collect();
            let kind = ort.map(|o| o.art);
            let sum = if matches!(kind, Some(DayKind::Ferien | DayKind::Uek)) {
                Summary { main: "–".to_string(), note: String::new(), level: "none" }
            } else {
                summary(day_total_tenths(&entries), ort.map_or(0, |o| o.soll_tenths()))
            };
            json!({
                "date": day.format("%Y-%m-%d").to_string(),
                "label": format!("{} {}", short_weekday(day), day.format("%d.%m.")),
                "weekday": german_weekday(day),
                "ort": ort.map_or("", |o| o.name.as_str()),
                "art": art_name(kind),
                "main": sum.main,
                "hours": if matches!(kind, Some(DayKind::Ferien | DayKind::Uek)) { String::new() } else { fmt_tenths(day_total_tenths(&entries)) },
                "percent": percent(day_total_tenths(&entries), ort.map_or(0, |o| o.soll_tenths())),
                "level": sum.level,
            })
        })
        .collect();

    // Eigene Wochentexte gehen vor den festen Texten; die Vorschau zeigt, was sonst im Export steht
    let week = build_week(date, file, config);
    let own = &file.texte;
    let preview = |own: &str, auto: &str| if own.trim().is_empty() { auto.to_string() } else { String::new() };
    let auto = week.auto.map(|k| if k == DayKind::Uek { "uek" } else { "ferien" });
    let name = (!config.nachname.is_empty() && !config.vorname.is_empty()).then(|| file_name(&week));
    json!({
        "date": date.format("%Y-%m-%d").to_string(),
        "label": format!("KW {kw} · {} – {}", monday.format("%d.%m."), sunday.checked_sub_days(Days::new(2)).unwrap_or(sunday).format("%d.%m.%Y")),
        "title": format!("Kalenderwoche {kw}"),
        "range": format!("{} – {}", monday.format("%d.%m."), sunday.checked_sub_days(Days::new(2)).unwrap_or(sunday).format("%d.%m.%Y")),
        "year": year,
        "kw": kw,
        "is_current": iso_week(today) == (year, kw),
        "plan": if overrides == 0 { "Plan: Standard".to_string() } else { format!("{overrides} Abweichung{}", if overrides == 1 { "" } else { "en" }) },
        "days": days,
        "orte": ort_names(plan),
        "ort_list": ort_list(plan),
        "texts": { "rueckblick": own.rueckblick, "reflexion": own.reflexion, "stimmung": own.stimmung },
        "preview": {
            "rueckblick": preview(&own.rueckblick, &week.review),
            "reflexion": preview(&own.reflexion, &week.reflection),
            "stimmung": preview(&own.stimmung, &week.mood),
        },
        "auto": auto,
        "file_name": name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use crate::export::tests::{at, config, date, override_day, week_file};
    use crate::journal::WeekTexts;

    // Juli 2026: 20 = Montag
    fn day(d: u32) -> NaiveDate {
        date(d)
    }

    #[test]
    fn labels_the_day() {
        assert_eq!(label(NaiveDate::from_ymd_opt(2026, 10, 7).unwrap()), "Mittwoch 07.10.");
        assert_eq!(short_weekday(day(20)), "Mo");
        assert_eq!(short_weekday(day(25)), "Sa");
    }

    #[test]
    fn shift_never_goes_past_today() {
        let today = day(22);
        assert_eq!(shift(today, -1, today), day(21));
        assert_eq!(shift(day(21), 1, today), today);
        assert_eq!(shift(today, 1, today), today);
        assert_eq!(shift(today, 0, today), today);
        assert_eq!(shift(NaiveDate::MIN, -1, today), NaiveDate::MIN);
        assert_eq!(shift(today, i64::MIN, today), today);
    }

    #[test]
    fn shift_weeks_goes_both_ways() {
        assert_eq!(shift_weeks(day(22), -1), day(15));
        assert_eq!(shift_weeks(day(22), 2), NaiveDate::from_ymd_opt(2026, 8, 5).unwrap());
        assert_eq!(shift_weeks(day(22), 0), day(22));
        assert_eq!(shift_weeks(NaiveDate::MAX, 1), NaiveDate::MAX);
        assert_eq!(shift_weeks(day(22), i64::MIN), day(22));
    }

    #[test]
    fn formats_tenths() {
        assert_eq!(fmt_tenths(64), "6.4");
        assert_eq!(fmt_tenths(0), "0.0");
        assert_eq!(fmt_tenths(5), "0.5");
        assert_eq!(fmt_tenths(-20), "-2.0");
        assert_eq!(fmt_tenths(240), "24.0");
    }

    #[test]
    fn summary_is_neutral_text() {
        let open = summary(64, 84);
        assert_eq!((open.main.as_str(), open.note.as_str(), open.level), ("6.4 / 8.4 h", "noch 2.0 h offen", "open"));
        let done = summary(84, 84);
        assert_eq!((done.main.as_str(), done.note.as_str(), done.level), ("8.4 / 8.4 h", "Soll erreicht", "done"));
        let over = summary(90, 84);
        assert_eq!((over.main.as_str(), over.note.as_str(), over.level), ("9.0 / 8.4 h", "0.6 h über dem Soll", "over"));
        let none = summary(25, 0);
        assert_eq!((none.main.as_str(), none.note.as_str(), none.level), ("2.5 h", "", "none"));
        assert_eq!(summary(0, 84).note, "noch 8.4 h offen");
    }

    #[test]
    fn entry_input_uses_duration_field_or_text_suffix() {
        assert_eq!(parse_entry_input("Bug gefixt 2.4", ""), Ok(("Bug gefixt".into(), 2.4)));
        assert_eq!(parse_entry_input("Bug gefixt", ""), Ok(("Bug gefixt".into(), 0.0)));
        assert_eq!(parse_entry_input("Bug gefixt", " 1,5 "), Ok(("Bug gefixt".into(), 1.5)));
        assert_eq!(parse_entry_input(" Bug ", "2h"), Ok(("Bug".into(), 2.0)));
        // das Dauerfeld hat Vorrang, der Text bleibt unangetastet
        assert_eq!(parse_entry_input("Release 2", "1"), Ok(("Release 2".into(), 1.0)));
        assert_eq!(parse_entry_input("x", "0"), Ok(("x".into(), 0.0)));
        for bad in ["viel", "-1", "25", "1.2.3", ".", "1e2", "NaN", "inf"] {
            assert!(parse_entry_input("x", bad).unwrap_err().contains("ungültig"), "{bad}");
        }
    }

    #[test]
    fn entry_time_keeps_today_and_moves_past_days() {
        let now = Local.with_ymd_and_hms(2026, 7, 22, 14, 30, 0).unwrap();
        assert_eq!(entry_time(day(22), now), now.with_timezone(&Utc));
        let past = entry_time(day(21), now).with_timezone(&Local);
        assert_eq!((past.date_naive(), past.format("%H:%M").to_string()), (day(21), "14:30".to_string()));
    }

    #[test]
    fn edit_text_appends_the_duration() {
        let e = Entry::manuell(at(20, 9, 0), "Kalender", 2.4);
        assert_eq!(edit_text(&e), "Kalender 2.4");
        assert_eq!(edit_text(&Entry::manuell(at(20, 9, 0), "Kalender", 0.0)), "Kalender");
        // der Text lässt sich wieder in dieselben Werte zerlegen
        assert_eq!(split_duration(&edit_text(&e)), ("Kalender".to_string(), 2.4));
    }

    #[test]
    fn bar_view_describes_the_day() {
        let hash = "a".repeat(40);
        let entries = vec![
            Entry::manuell(at(22, 9, 5), "Zeile 1\nZeile 2", 2.4),
            Entry::commit(at(22, 10, 0), "recur", &hash, "Fix"),
        ];
        let plan = config().plan;
        let v = bar_view(day(22), day(22), &entries, None, &plan, false);
        assert_eq!(v["date"], "2026-07-22");
        assert_eq!(v["label"], "Mittwoch 22.07.");
        assert_eq!(v["short"], "Mi 22.07.");
        assert_eq!(v["title"], "Mittwoch, 22.07.");
        assert_eq!(v["kw"], 30);
        assert_eq!((v["is_today"].as_bool(), v["can_next"].as_bool()), (Some(true), Some(false)));
        assert_eq!((v["ort"].as_str(), v["main"].as_str(), v["note"].as_str()), (Some("Noser Young"), Some("2.4 / 8.4 h"), Some("noch 6.0 h offen")));
        assert_eq!((v["level"].as_str(), v["can_rest"].as_bool(), v["collapsed"].as_bool()), (Some("open"), Some(true), Some(false)));
        assert_eq!(v["orte"].as_array().unwrap().len(), 4);

        let e = v["entries"].as_array().unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!((e[0]["commit"].as_bool(), e[0]["text"].as_str(), e[0]["edit"].as_str()), (Some(false), Some("Zeile 1\nZeile 2"), Some("Zeile 1\nZeile 2 2.4")));
        assert_eq!((e[0]["hours"].as_f64(), e[0]["time"].as_str()), (Some(2.4), Some("09:05")));
        assert_eq!((e[1]["commit"].as_bool(), e[1]["repo"].as_str()), (Some(true), Some("recur")));
        assert!(e.iter().all(|x| x["id"].is_string()));

        // vergangener Tag und Override
        let v = bar_view(day(21), day(22), &[], Some("Ferien"), &plan, true);
        assert_eq!((v["can_next"].as_bool(), v["ort"].as_str(), v["collapsed"].as_bool(), v["can_rest"].as_bool()), (Some(true), Some("Ferien"), Some(true), Some(false)));
        // Wochenende ohne Ort: kein Soll
        let v = bar_view(day(25), day(25), &[], None, &plan, false);
        assert_eq!((v["ort"].as_str(), v["main"].as_str(), v["level"].as_str()), (Some(""), Some("0.0 h"), Some("none")));
    }

    #[test]
    fn short_note_and_percent() {
        assert_eq!(short_note(64, 84), "noch 2.0 h");
        assert_eq!(short_note(84, 84), "Soll erreicht");
        assert_eq!(short_note(92, 84), "+0.8 h zu viel");
        assert_eq!(short_note(10, 0), "");
        assert_eq!((percent(64, 84), percent(84, 84), percent(92, 84), percent(0, 84), percent(5, 0)), (76, 100, 100, 0, 0));
        assert_eq!(percent(-5, 84), 0);
    }

    #[test]
    fn no_hours_rules() {
        use DayKind::*;
        assert!(no_hours(Some(Uek), 0, 0) && no_hours(Some(Ferien), 84, 30) && no_hours(None, 0, 0));
        // Gibb: ohne eingetragene Stunden der Hinweis, mit Stunden der Balken
        assert!(no_hours(Some(Schule), 84, 0));
        assert!(!no_hours(Some(Schule), 84, 30));
        assert!(no_hours(Some(Schule), 0, 30));
        assert!(!no_hours(Some(Arbeit), 84, 0));
        assert!(no_hours(Some(Arbeit), 0, 0));
    }

    #[test]
    fn bar_view_carries_art_percent_and_pill_flags() {
        let plan = config().plan;
        let entries = vec![Entry::manuell(at(22, 9, 5), "A", 6.4)];
        let v = bar_view(day(22), day(22), &entries, None, &plan, false);
        assert_eq!((v["art"].as_str(), v["percent"].as_i64(), v["short_note"].as_str()), (Some("arbeit"), Some(76), Some("noch 2.0 h")));
        assert_eq!((v["soll"].as_str(), v["total"].as_str(), v["no_hours"].as_bool()), (Some("8.4"), Some("6.4"), Some(false)));
        assert_eq!(v["ort_list"][0], json!({ "name": "Noser Young", "art": "arbeit" }));
        assert_eq!(v["ort_list"][1]["art"], "schule");
        assert_eq!(v["ort_list"][2]["art"], "uek");
        assert_eq!(v["ort_list"][3]["art"], "ferien");

        // über Soll
        let over = vec![Entry::manuell(at(22, 9, 5), "A", 9.2)];
        let v = bar_view(day(22), day(22), &over, None, &plan, false);
        assert_eq!((v["level"].as_str(), v["short_note"].as_str(), v["percent"].as_i64()), (Some("over"), Some("+0.8 h zu viel"), Some(100)));

        // üK und Ferien: keine Stunden, Hinweis statt Balken
        let v = bar_view(day(22), day(22), &[], Some("üK"), &plan, false);
        assert_eq!((v["art"].as_str(), v["no_hours"].as_bool()), (Some("uek"), Some(true)));
        let v = bar_view(day(22), day(22), &[], Some("Ferien"), &plan, false);
        assert_eq!((v["art"].as_str(), v["no_hours"].as_bool()), (Some("ferien"), Some(true)));
        // Gibb ohne Stunden
        let v = bar_view(day(22), day(22), &[], Some("Gibb"), &plan, false);
        assert_eq!((v["art"].as_str(), v["no_hours"].as_bool()), (Some("schule"), Some(true)));
        // Wochenende ohne Ort
        let v = bar_view(day(25), day(25), &[], None, &plan, false);
        assert_eq!((v["art"].as_str(), v["no_hours"].as_bool()), (Some(""), Some(true)));
    }

    #[test]
    fn bar_view_counts_commits_only_with_duration() {
        let hash = "b".repeat(40);
        let mut commit = Entry::commit(at(22, 10, 0), "recur", &hash, "Fix");
        let plan = config().plan;
        let v = bar_view(day(22), day(22), std::slice::from_ref(&commit), None, &plan, false);
        assert_eq!((v["main"].as_str(), v["can_rest"].as_bool()), (Some("0.0 / 8.4 h"), Some(false)));
        commit.stunden = 1.5;
        let v = bar_view(day(22), day(22), &[commit], None, &plan, false);
        assert_eq!((v["main"].as_str(), v["can_rest"].as_bool()), (Some("1.5 / 8.4 h"), Some(true)));
    }

    #[test]
    fn bar_view_survives_hostile_text() {
        let evil = "</script><img src=x onerror=alert(1)> \" ' \\ \u{2028}";
        let plan = config().plan;
        let v = bar_view(day(22), day(22), &[Entry::manuell(at(22, 9, 0), evil, 0.0)], None, &plan, false);
        // wird als JSON-Text übergeben und im Fenster per textContent gesetzt
        let back: Value = serde_json::from_str(&v.to_string()).unwrap();
        assert_eq!(back["entries"][0]["text"], evil);
    }

    #[test]
    fn week_view_has_five_cards_and_the_file_name() {
        let file = week_file(vec![Entry::manuell(at(20, 9, 0), "A", 8.4), Entry::manuell(at(22, 9, 0), "B", 6.4)]);
        let v = week_view(day(22), day(22), &file, &config());
        assert_eq!(v["label"], "KW 30 · 20.07. – 24.07.2026");
        assert_eq!((v["kw"].as_u64(), v["is_current"].as_bool(), v["plan"].as_str()), (Some(30), Some(true), Some("Plan: Standard")));
        assert_eq!(v["file_name"], "Arbeitsjournal-maurer-jemuel-2026-30.docx");
        assert_eq!(v["auto"], Value::Null);
        assert_eq!((v["title"].as_str(), v["range"].as_str()), (Some("Kalenderwoche 30"), Some("20.07. – 24.07.2026")));
        let days = v["days"].as_array().unwrap();
        assert_eq!(days.len(), 5);
        assert_eq!((days[0]["label"].as_str(), days[0]["main"].as_str(), days[0]["level"].as_str()), (Some("Mo 20.07."), Some("8.4 / 8.4 h"), Some("done")));
        assert_eq!((days[2]["main"].as_str(), days[2]["ort"].as_str()), (Some("6.4 / 8.4 h"), Some("Noser Young")));
        assert_eq!(days[4]["date"], "2026-07-24");
        assert_eq!((days[0]["hours"].as_str(), days[0]["art"].as_str(), days[0]["percent"].as_i64()), (Some("8.4"), Some("arbeit"), Some(100)));
        assert_eq!((days[2]["hours"].as_str(), days[2]["percent"].as_i64()), (Some("6.4"), Some(76)));
        assert_eq!(days[3]["hours"], "0.0");
        assert!(["arbeit", "schule"].contains(&days[3]["art"].as_str().unwrap()));

        // andere Woche ist nicht die aktuelle
        assert_eq!(week_view(day(22), day(29), &file, &config())["is_current"], false);
    }

    #[test]
    fn week_view_without_name_has_no_file_name() {
        let mut c = config();
        c.nachname.clear();
        assert_eq!(week_view(day(22), day(22), &week_file(vec![]), &c)["file_name"], Value::Null);
    }

    #[test]
    fn week_view_counts_overrides_and_previews_auto_texts() {
        let mut file = week_file(vec![]);
        override_day(&mut file, 21, "Gibb");
        let v = week_view(day(22), day(22), &file, &config());
        assert_eq!(v["plan"], "1 Abweichung");
        assert_eq!(v["days"][1]["ort"], "Gibb");

        // reine üK-Woche: die festen Texte erscheinen als Vorschau, eigene Texte gehen vor
        for d in 20..=24 {
            override_day(&mut file, d, "üK");
        }
        let v = week_view(day(22), day(22), &file, &config());
        assert_eq!(v["plan"], "5 Abweichungen");
        assert_eq!(v["auto"], "uek");
        assert_eq!(v["preview"]["reflexion"], "Diese Woche gibt es keine Wochenreflexion, da wir einen üK hatten.");
        assert_eq!(v["days"][0]["main"], "–");
        file.texte = WeekTexts { rueckblick: "Eigener Rückblick".into(), ..WeekTexts::default() };
        let v = week_view(day(22), day(22), &file, &config());
        assert_eq!((v["texts"]["rueckblick"].as_str(), v["preview"]["rueckblick"].as_str()), (Some("Eigener Rückblick"), Some("")));
        assert!(!v["preview"]["stimmung"].as_str().unwrap().is_empty());

        // Ferien-Woche
        let mut file = week_file(vec![]);
        for d in 20..=24 {
            override_day(&mut file, d, "Ferien");
        }
        assert_eq!(week_view(day(22), day(22), &file, &config())["auto"], "ferien");
    }

    #[test]
    fn week_numbers_follow_iso() {
        // 29.12.2025 (Montag) gehört zu KW 1 2026
        let d = NaiveDate::from_ymd_opt(2025, 12, 31).unwrap();
        assert_eq!(week_view(d, d, &WeekFile::default(), &config())["kw"], 1);
        assert_eq!(d.weekday(), chrono::Weekday::Wed);
    }
}
