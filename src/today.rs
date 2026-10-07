use crate::export::german_weekday;
use crate::journal::{DayKind, Entry};
use chrono::{Days, Local, NaiveDate};
use serde_json::{Value, json};

pub fn label(date: NaiveDate) -> String {
    format!("{}, {}", german_weekday(date), date.format("%d.%m.%Y"))
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

fn kind_label(kind: DayKind) -> &'static str {
    match kind {
        DayKind::Arbeit => "Arbeit",
        DayKind::Schule => "Schule",
        DayKind::Uek => "ÜK",
        DayKind::Ferien => "Ferien",
        DayKind::Krank => "Krank",
    }
}

fn entry_view(entry: &Entry) -> Value {
    let time = entry.t().with_timezone(&Local).format("%H:%M").to_string();
    match entry {
        Entry::Note { id, text, source, .. } => {
            json!({ "id": id, "type": "note", "time": time, "text": text, "source": source })
        }
        Entry::Commit { id, repo, hash, text, .. } => {
            json!({ "id": id, "type": "commit", "time": time, "repo": repo, "hash": hash, "text": text })
        }
        Entry::Day { id, kind, .. } => {
            json!({ "id": id, "type": "day", "time": time, "kind": kind_label(*kind) })
        }
        Entry::Reflection { id, review, reflection, mood, .. } => json!({
            "id": id, "type": "reflection", "time": time,
            "review": review, "reflection": reflection, "mood": mood,
        }),
    }
}

/// Daten für das Fenster "Heutige Einträge" (`entries` nach Zeit sortiert)
pub fn view(date: NaiveDate, today: NaiveDate, entries: &[Entry]) -> Value {
    json!({
        "date": date.format("%Y-%m-%d").to_string(),
        "label": label(date),
        "today": date >= today,
        "entries": entries.iter().map(entry_view).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn date(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
    }

    fn at(d: u32, h: u32, m: u32) -> chrono::DateTime<Utc> {
        Local.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn labels_the_day() {
        assert_eq!(label(date(7)), "Mittwoch, 07.10.2026");
    }

    #[test]
    fn shift_never_goes_past_today() {
        let today = date(7);
        assert_eq!(shift(today, -1, today), date(6));
        assert_eq!(shift(date(6), 1, today), today);
        assert_eq!(shift(today, 1, today), today);
        assert_eq!(shift(today, -30, today), NaiveDate::from_ymd_opt(2026, 9, 7).unwrap());
        assert_eq!(shift(today, 0, today), today);
        assert_eq!(shift(NaiveDate::MIN, -1, today), NaiveDate::MIN);
        assert_eq!(shift(today, i64::MIN, today), today);
    }

    #[test]
    fn view_describes_every_entry_type() {
        let hash = "a".repeat(40);
        let entries = vec![
            Entry::day(at(7, 7, 30), DayKind::Uek),
            Entry::note(at(7, 9, 5), "Zeile 1\nZeile 2", "checkin"),
            Entry::commit(at(7, 10, 0), "recur", &hash, "Fix"),
            Entry::reflection(at(7, 16, 45), "Rückblick", "Reflexion", "gut"),
        ];
        let v = view(date(7), date(7), &entries);
        assert_eq!(v["date"], "2026-10-07");
        assert_eq!(v["label"], "Mittwoch, 07.10.2026");
        assert_eq!(v["today"], true);

        let e = v["entries"].as_array().unwrap();
        assert_eq!(e.len(), 4);
        assert_eq!((e[0]["type"].as_str(), e[0]["kind"].as_str(), e[0]["time"].as_str()), (Some("day"), Some("ÜK"), Some("07:30")));
        assert_eq!((e[1]["type"].as_str(), e[1]["text"].as_str(), e[1]["time"].as_str()), (Some("note"), Some("Zeile 1\nZeile 2"), Some("09:05")));
        assert_eq!((e[2]["repo"].as_str(), e[2]["text"].as_str()), (Some("recur"), Some("Fix")));
        assert_eq!((e[3]["review"].as_str(), e[3]["reflection"].as_str(), e[3]["mood"].as_str()), (Some("Rückblick"), Some("Reflexion"), Some("gut")));
        assert!(e.iter().all(|x| x["id"].is_string()));

        assert_eq!(view(date(6), date(7), &[])["today"], false);
        assert!(view(date(6), date(7), &[])["entries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn view_survives_hostile_text() {
        let evil = "</script><img src=x onerror=alert(1)> \" ' \\ \u{2028}";
        let v = view(date(7), date(7), &[Entry::note(at(7, 9, 0), evil, "quick")]);
        // wird als JSON-Text übergeben und im Fenster per textContent gesetzt
        let json = v.to_string();
        let back: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(back["entries"][0]["text"], evil);
    }
}
