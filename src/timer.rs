use crate::journal::DayKind;
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

pub const SNOOZE_MINUTES: i64 = 15;
/// Ein Zeitpunkt, der länger her ist (z. B. nach dem Standby), gilt als verpasst statt als fällig
const FRESH_MINUTES: i64 = 2;

/// Geprüfte Arbeitszeit-Einstellungen für den Timer
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub work_days: Vec<Weekday>,
    pub blocks: Vec<(NaiveTime, NaiveTime)>,
    pub interval_minutes: u32,
    pub away_minutes: u32,
    pub school_days: Vec<Weekday>,
    pub reflection_day: Weekday,
}

impl Schedule {
    /// Weg heisst: Sitzung gesperrt oder im Standby, oder zu lange keine Eingabe
    pub fn is_away(&self, idle: std::time::Duration, locked: bool) -> bool {
        locked || idle >= std::time::Duration::from_secs(u64::from(self.away_minutes) * 60)
    }
}

/// Art des Tages: die letzte Markierung, sonst wiederkehrender Schultag, sonst Arbeitstag.
/// `None` heisst frei (kein Arbeitstag, keine Markierung).
pub fn effective_kind(date: NaiveDate, marked: Option<DayKind>, schedule: &Schedule) -> Option<DayKind> {
    if marked.is_some() {
        return marked;
    }
    let weekday = date.weekday();
    if schedule.school_days.contains(&weekday) {
        Some(DayKind::Schule)
    } else if schedule.work_days.contains(&weekday) {
        Some(DayKind::Arbeit)
    } else {
        None
    }
}

pub fn checkins_allowed(date: NaiveDate, marked: Option<DayKind>, schedule: &Schedule) -> bool {
    effective_kind(date, marked, schedule) == Some(DayKind::Arbeit)
}

fn minutes(t: NaiveTime) -> u32 {
    t.hour() * 60 + t.minute()
}

/// Fällige Zeitpunkte eines Tages: in jedem Block alle `interval` Minuten nach dem Beginn,
/// zusätzlich das Blockende.
pub fn due_points(schedule: &Schedule, date: NaiveDate) -> Vec<NaiveDateTime> {
    let step = schedule.interval_minutes.max(1);
    let mut points = Vec::new();
    for &(start, end) in &schedule.blocks {
        let (start, end) = (minutes(start), minutes(end));
        let mut m = start + step;
        while m <= end {
            points.push(m);
            m += step;
        }
        if points.last() != Some(&end) && end > start {
            points.push(end);
        }
    }
    points
        .into_iter()
        .filter_map(|m| date.and_hms_opt(m / 60, m % 60, 0))
        .collect()
}

/// Nach `now` kommt an diesem Tag kein weiterer Zeitpunkt mehr
pub fn is_last_checkin(schedule: &Schedule, now: NaiveDateTime) -> bool {
    let points = due_points(schedule, now.date());
    !points.is_empty() && points.iter().all(|p| *p <= now)
}

/// Am Reflexionstag fragt der letzte Check-in nach der Wochenreflexion, wenn es diese Woche noch keine gibt
pub fn reflection_wanted(schedule: &Schedule, now: NaiveDateTime, week_has_reflection: bool) -> bool {
    now.weekday() == schedule.reflection_day
        && !week_has_reflection
        && is_last_checkin(schedule, now)
}

pub fn in_work_time(schedule: &Schedule, t: NaiveTime) -> bool {
    schedule.blocks.iter().any(|&(start, end)| t >= start && t <= end)
}

fn latest_point(schedule: &Schedule, now: NaiveDateTime) -> Option<NaiveDateTime> {
    due_points(schedule, now.date()).into_iter().filter(|p| *p <= now).max()
}

/// Entscheidet, wann ein Check-in fällig wird. Zeit, Leerlauf und Tagesart kommen von aussen.
pub struct Timer {
    schedule: Schedule,
    handled: Option<NaiveDateTime>,
    missed: Option<NaiveDate>,
    snooze_until: Option<NaiveDateTime>,
}

impl Timer {
    /// Zeitpunkte, die schon vorbei sind, lösen beim Start nichts aus
    pub fn new(schedule: Schedule, now: NaiveDateTime) -> Self {
        let handled = latest_point(&schedule, now);
        Self { schedule, handled, missed: None, snooze_until: None }
    }

    pub fn snooze(&mut self, now: NaiveDateTime) {
        self.snooze_until = Some(now + Duration::minutes(SNOOZE_MINUTES));
    }

    /// Der Eintrag wurde geschrieben oder übersprungen
    pub fn answered(&mut self) {
        self.snooze_until = None;
        self.missed = None;
    }

    /// `true`, wenn jetzt ein Check-in fällig wird
    pub fn tick(&mut self, now: NaiveDateTime, away: bool, marked: Option<DayKind>) -> bool {
        let allowed = checkins_allowed(now.date(), marked, &self.schedule);
        let inside = allowed && in_work_time(&self.schedule, now.time());

        let mut wanted = false;
        let mut stale = false;
        if let Some(point) = latest_point(&self.schedule, now)
            && self.handled.is_none_or(|h| point > h)
        {
            self.handled = Some(point);
            let fresh = now - point < Duration::minutes(FRESH_MINUTES);
            wanted = allowed && fresh;
            stale = allowed && !fresh;
        }

        if !allowed {
            self.missed = None;
            self.snooze_until = None;
            return false;
        }
        if self.snooze_until.is_some_and(|s| now >= s) {
            self.snooze_until = None;
            wanted |= inside;
        }
        if self.missed.is_some_and(|d| d != now.date()) {
            self.missed = None;
        }

        let mut fire = false;
        if stale {
            self.missed = Some(now.date());
        }
        if wanted {
            if away {
                self.missed = Some(now.date());
            } else {
                fire = true;
            }
        }
        // Zurück nach der Abwesenheit: höchstens ein Check-in, nicht mehrere nachgeholte
        if self.missed.is_some() && !away && inside {
            fire = true;
        }
        if fire {
            self.missed = None;
            self.snooze_until = None;
        }
        fire
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn schedule() -> Schedule {
        Settings::default().schedule().unwrap()
    }

    fn dt(d: u32, h: u32, m: u32, s: u32) -> NaiveDateTime {
        // Oktober 2026: 5 = Montag, 7 = Mittwoch, 10 = Samstag
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap().and_hms_opt(h, m, s).unwrap()
    }

    fn t(h: u32, m: u32, s: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, s).unwrap()
    }

    /// Wie `tick`, nur mit Mo 5.10. als Standardtag und anwesend
    fn run(timer: &mut Timer, now: NaiveDateTime, away: bool) -> bool {
        timer.tick(now, away, None)
    }

    #[test]
    fn due_points_per_block() {
        let points = due_points(&schedule(), dt(5, 0, 0, 0).date());
        let times: Vec<_> = points.iter().map(|p| p.format("%H:%M").to_string()).collect();
        assert_eq!(times, ["09:00", "10:00", "11:00", "12:00", "14:00", "15:00", "16:00", "17:00"]);
    }

    #[test]
    fn due_points_add_block_end_when_interval_does_not_fit() {
        let mut s = schedule();
        s.interval_minutes = 90;
        let points = due_points(&s, dt(5, 0, 0, 0).date());
        let times: Vec<_> = points.iter().map(|p| p.format("%H:%M").to_string()).collect();
        assert_eq!(times, ["09:30", "11:00", "12:00", "14:30", "16:00", "17:00"]);
    }

    #[test]
    fn work_time_edges() {
        let s = schedule();
        assert!(!in_work_time(&s, t(7, 59, 59)));
        assert!(in_work_time(&s, t(8, 0, 0)));
        assert!(in_work_time(&s, t(12, 0, 0)));
        assert!(!in_work_time(&s, t(12, 0, 1)));
        assert!(!in_work_time(&s, t(12, 30, 0)));
        assert!(in_work_time(&s, t(13, 0, 0)));
        assert!(in_work_time(&s, t(17, 0, 0)));
        assert!(!in_work_time(&s, t(17, 0, 1)));
    }

    #[test]
    fn fires_at_each_interval() {
        let mut timer = Timer::new(schedule(), dt(5, 8, 0, 0));
        assert!(!run(&mut timer, dt(5, 8, 30, 0), false));
        assert!(!run(&mut timer, dt(5, 8, 59, 59), false));
        assert!(run(&mut timer, dt(5, 9, 0, 0), false));
        // derselbe Zeitpunkt löst nicht zweimal aus
        assert!(!run(&mut timer, dt(5, 9, 0, 15), false));
        assert!(!run(&mut timer, dt(5, 9, 45, 0), false));
        assert!(run(&mut timer, dt(5, 10, 0, 10), false));
    }

    #[test]
    fn block_end_fires_and_nothing_in_the_break() {
        let mut timer = Timer::new(schedule(), dt(5, 11, 30, 0));
        assert!(run(&mut timer, dt(5, 12, 0, 0), false));
        assert!(!run(&mut timer, dt(5, 12, 30, 0), false));
        assert!(!run(&mut timer, dt(5, 13, 0, 0), false));
        assert!(run(&mut timer, dt(5, 14, 0, 0), false));
        assert!(run(&mut timer, dt(5, 17, 0, 0), false));
        assert!(!run(&mut timer, dt(5, 18, 0, 0), false));
    }

    #[test]
    fn start_in_the_middle_of_the_day_does_not_fire_at_once() {
        let mut timer = Timer::new(schedule(), dt(5, 10, 30, 0));
        assert!(!run(&mut timer, dt(5, 10, 30, 15), false));
        assert!(run(&mut timer, dt(5, 11, 0, 0), false));
    }

    #[test]
    fn no_checkins_outside_work_days() {
        let mut timer = Timer::new(schedule(), dt(10, 8, 0, 0));
        assert!(!run(&mut timer, dt(10, 9, 0, 0), false));
        assert!(!run(&mut timer, dt(10, 10, 0, 0), false));
    }

    #[test]
    fn marked_days_decide() {
        let mut s = schedule();
        s.school_days = vec![Weekday::Wed];
        // Mittwoch ist Schule, wenn nichts anderes markiert ist
        let mut timer = Timer::new(s.clone(), dt(7, 8, 0, 0));
        assert!(!timer.tick(dt(7, 9, 0, 0), false, None));
        // Markierung Arbeit überschreibt den Schultag
        let mut timer = Timer::new(s.clone(), dt(7, 8, 0, 0));
        assert!(timer.tick(dt(7, 9, 0, 0), false, Some(DayKind::Arbeit)));
        // Krank, Ferien, ÜK: keine Check-ins
        for kind in [DayKind::Krank, DayKind::Ferien, DayKind::Uek, DayKind::Schule] {
            let mut timer = Timer::new(s.clone(), dt(5, 8, 0, 0));
            assert!(!timer.tick(dt(5, 9, 0, 0), false, Some(kind)), "{kind:?}");
        }
        // Eine Markierung am Samstag macht ihn zum Arbeitstag
        let mut timer = Timer::new(s, dt(10, 8, 0, 0));
        assert!(timer.tick(dt(10, 9, 0, 0), false, Some(DayKind::Arbeit)));
    }

    #[test]
    fn marking_krank_midday_stops_the_day() {
        let mut timer = Timer::new(schedule(), dt(5, 8, 0, 0));
        assert!(run(&mut timer, dt(5, 9, 0, 0), false));
        assert!(!timer.tick(dt(5, 10, 0, 0), false, Some(DayKind::Krank)));
        // Wieder Arbeit: der verpasste Zeitpunkt von 10:00 wird nicht nachgeholt
        assert!(!timer.tick(dt(5, 10, 5, 0), false, Some(DayKind::Arbeit)));
        assert!(timer.tick(dt(5, 11, 0, 0), false, Some(DayKind::Arbeit)));
    }

    #[test]
    fn away_at_due_time_asks_once_on_return() {
        let mut timer = Timer::new(schedule(), dt(5, 8, 0, 0));
        // 09:00, 10:00 und 11:00 verstreichen, während ich weg bin
        assert!(!run(&mut timer, dt(5, 9, 0, 0), true));
        assert!(!run(&mut timer, dt(5, 10, 0, 0), true));
        assert!(!run(&mut timer, dt(5, 11, 0, 0), true));
        assert!(!run(&mut timer, dt(5, 11, 20, 0), true));
        // Rückkehr: genau ein Check-in
        assert!(run(&mut timer, dt(5, 11, 30, 0), false));
        assert!(!run(&mut timer, dt(5, 11, 30, 15), false));
        assert!(!run(&mut timer, dt(5, 11, 45, 0), false));
        // und danach geht es normal weiter
        assert!(run(&mut timer, dt(5, 12, 0, 0), false));
    }

    #[test]
    fn return_outside_work_time_asks_nothing() {
        let mut timer = Timer::new(schedule(), dt(5, 15, 30, 0));
        assert!(!run(&mut timer, dt(5, 16, 0, 0), true));
        assert!(!run(&mut timer, dt(5, 17, 0, 0), true));
        assert!(!run(&mut timer, dt(5, 17, 30, 0), false));
        // am nächsten Tag ist der verpasste Zeitpunkt vergessen
        assert!(!timer.tick(dt(6, 8, 0, 0), false, None));
        assert!(timer.tick(dt(6, 9, 0, 0), false, None));
    }

    #[test]
    fn away_over_lunch_asks_after_lunch() {
        let mut timer = Timer::new(schedule(), dt(5, 11, 30, 0));
        assert!(!run(&mut timer, dt(5, 12, 0, 0), true));
        assert!(!run(&mut timer, dt(5, 12, 30, 0), true));
        assert!(run(&mut timer, dt(5, 13, 5, 0), false));
    }

    #[test]
    fn snooze_fires_after_fifteen_minutes() {
        let mut timer = Timer::new(schedule(), dt(5, 8, 0, 0));
        assert!(run(&mut timer, dt(5, 9, 0, 0), false));
        timer.snooze(dt(5, 9, 5, 0));
        assert!(!run(&mut timer, dt(5, 9, 19, 59), false));
        assert!(run(&mut timer, dt(5, 9, 20, 0), false));
        assert!(!run(&mut timer, dt(5, 9, 20, 15), false));
    }

    #[test]
    fn snooze_while_away_waits_for_return() {
        let mut timer = Timer::new(schedule(), dt(5, 8, 0, 0));
        timer.snooze(dt(5, 9, 5, 0));
        assert!(!run(&mut timer, dt(5, 9, 20, 0), true));
        assert!(!run(&mut timer, dt(5, 9, 40, 0), true));
        assert!(run(&mut timer, dt(5, 9, 50, 0), false));
    }

    #[test]
    fn snooze_outside_work_time_is_dropped() {
        let mut timer = Timer::new(schedule(), dt(5, 8, 0, 0));
        timer.snooze(dt(5, 16, 55, 0));
        assert!(run(&mut timer, dt(5, 17, 0, 0), false));
        assert!(!run(&mut timer, dt(5, 17, 10, 0), false));

        let mut timer = Timer::new(schedule(), dt(5, 16, 50, 0));
        timer.snooze(dt(5, 17, 50, 0));
        assert!(!run(&mut timer, dt(5, 18, 5, 0), false));
    }

    #[test]
    fn answering_cancels_snooze_and_missed() {
        let mut timer = Timer::new(schedule(), dt(5, 8, 0, 0));
        assert!(run(&mut timer, dt(5, 9, 0, 0), false));
        timer.snooze(dt(5, 9, 5, 0));
        timer.answered();
        assert!(!run(&mut timer, dt(5, 9, 20, 0), false));

        assert!(!run(&mut timer, dt(5, 10, 0, 0), true));
        timer.answered();
        assert!(!run(&mut timer, dt(5, 10, 30, 0), false));
    }

    #[test]
    fn long_gap_after_standby_asks_at_most_once() {
        let mut timer = Timer::new(schedule(), dt(5, 8, 30, 0));
        // Rechner schläft von 08:30 bis 11:40, die Zeitpunkte 09:00 bis 11:00 sind lange her
        assert!(run(&mut timer, dt(5, 11, 40, 0), false));
        assert!(!run(&mut timer, dt(5, 11, 40, 15), false));
        assert!(run(&mut timer, dt(5, 12, 0, 0), false));

        // Aufwachen nach Feierabend: nichts
        let mut timer = Timer::new(schedule(), dt(5, 16, 30, 0));
        assert!(!run(&mut timer, dt(5, 18, 5, 0), false));
    }

    #[test]
    fn away_threshold_and_lock() {
        let s = schedule();
        let min = std::time::Duration::from_secs(60);
        assert!(!s.is_away(min * 9, false));
        assert!(s.is_away(min * 10, false));
        assert!(s.is_away(std::time::Duration::ZERO, true));
    }

    #[test]
    fn last_checkin_of_the_day() {
        let s = schedule();
        assert!(!is_last_checkin(&s, dt(5, 12, 0, 0)));
        assert!(!is_last_checkin(&s, dt(5, 16, 59, 0)));
        assert!(is_last_checkin(&s, dt(5, 17, 0, 0)));
        assert!(is_last_checkin(&s, dt(5, 17, 0, 30)));
        assert!(!is_last_checkin(&s, dt(5, 8, 0, 0)));
    }

    #[test]
    fn reflection_only_at_the_last_checkin_of_the_reflection_day() {
        let s = schedule();
        // Oktober 2026: 9 = Freitag (Standard-Reflexionstag), 8 = Donnerstag
        assert!(reflection_wanted(&s, dt(9, 17, 0, 0), false));
        assert!(!reflection_wanted(&s, dt(9, 16, 0, 0), false));
        assert!(!reflection_wanted(&s, dt(9, 12, 0, 0), false));
        assert!(!reflection_wanted(&s, dt(8, 17, 0, 0), false));
        // diese Woche schon geschrieben
        assert!(!reflection_wanted(&s, dt(9, 17, 0, 0), true));

        let mut s = s;
        s.reflection_day = Weekday::Thu;
        assert!(reflection_wanted(&s, dt(8, 17, 0, 0), false));
        assert!(!reflection_wanted(&s, dt(9, 17, 0, 0), false));
    }

    #[test]
    fn effective_kind_rules() {
        let mut s = schedule();
        s.school_days = vec![Weekday::Wed];
        let d = |day| dt(day, 0, 0, 0).date();
        assert_eq!(effective_kind(d(5), None, &s), Some(DayKind::Arbeit));
        assert_eq!(effective_kind(d(7), None, &s), Some(DayKind::Schule));
        assert_eq!(effective_kind(d(10), None, &s), None);
        assert_eq!(effective_kind(d(7), Some(DayKind::Arbeit), &s), Some(DayKind::Arbeit));
        assert_eq!(effective_kind(d(5), Some(DayKind::Ferien), &s), Some(DayKind::Ferien));
    }
}
