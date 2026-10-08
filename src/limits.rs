//! Claude-Limits für die Pille: liest `claude-limits.json` (geschrieben von der Statuszeile von Claude Code),
//! bereitet Prozente, Warnstufen und Reset-Texte auf. Alles Rechnen und Formatieren ist rein (Zeit und
//! Zeitzonen-Offset kommen von aussen), nur `LimitsWatcher` fasst das Dateisystem an.

use crate::log;
use crate::store;
use chrono::{DateTime, Datelike, Local, NaiveDateTime, TimeZone, Weekday};
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::time::SystemTime;

/// Ist die ganze Datei älter, gilt es als keine Daten
pub const STALE_AFTER_SECS: i64 = 6 * 3600;
/// Standard-Schwellen: ab 80 % färbt sich ein Ring gelb, ab 95 % rot
pub const DEFAULT_WARN_AT: u32 = 80;
pub const DEFAULT_CRIT_AT: u32 = 95;

/// Ab wie viel Prozent ein Ring gelb (`warn`) bzw. rot (`crit`) wird; gültig ist `1 <= warn < crit <= 100`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Thresholds {
    pub warn: u32,
    pub crit: u32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { warn: DEFAULT_WARN_AT, crit: DEFAULT_CRIT_AT }
    }
}

impl Thresholds {
    /// Prüft die Eingabe; der Fehler sagt, was nicht stimmt
    pub fn check(warn: u32, crit: u32) -> Result<Self, String> {
        if !(1..=100).contains(&warn) || !(1..=100).contains(&crit) {
            return Err("Die Schwellen für Warnung und Kritisch müssen zwischen 1 und 100 % liegen".to_string());
        }
        if warn >= crit {
            return Err(format!("Die Warnschwelle ({warn} %) muss unter der kritischen Schwelle ({crit} %) liegen"));
        }
        Ok(Self { warn, crit })
    }

    /// Aus den gespeicherten Zahlen. Ungültige Werte (z. B. von Hand in settings.json) werden durch die Standards ersetzt.
    pub fn resolve(warn: u32, crit: u32) -> Self {
        Self::check(warn, crit).unwrap_or_default()
    }
}
/// Radien der drei Ringe in der Pille (viewBox 44) und in der Karte (viewBox 40)
pub const PILL_RADII: [f64; 3] = [19.5, 14.0, 8.5];
pub const CARD_RADIUS: f64 = 16.0;

pub fn path() -> PathBuf {
    store::data_dir().join("claude-limits.json")
}

// --- Ringfarben ---

pub const DEFAULT_FIVE_HOUR: &str = "#ededed";
pub const DEFAULT_SEVEN_DAY: &str = "#8a8a8a";
pub const DEFAULT_CONTEXT: &str = "#5c5c5c";
pub const DEFAULT_WARN: &str = "#d6b878";
pub const DEFAULT_CRIT: &str = "#d28f8f";
/// Farbe der Spur (Hintergrundring), fest
pub const TRACK_COLOR: &str = "#1c1c1c";
/// Unter diesem Kontrastverhältnis zur Spur ist ein Ring kaum zu sehen
pub const MIN_RING_CONTRAST: f64 = 1.5;

/// "#RRGGBB" oder "#RGB" (mit oder ohne `#`, Gross-/Kleinschreibung egal, Leerraum aussen ignoriert)
/// wird zu "#rrggbb" in Kleinbuchstaben. Alles andere ist `None`.
pub fn normalize_hex(text: &str) -> Option<String> {
    let t = text.trim();
    let digits = t.strip_prefix('#').unwrap_or(t);
    if !digits.is_ascii() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match digits.len() {
        6 => Some(format!("#{}", digits.to_ascii_lowercase())),
        3 => {
            let mut out = String::from("#");
            for c in digits.chars() {
                let c = c.to_ascii_lowercase();
                out.push(c);
                out.push(c);
            }
            Some(out)
        }
        _ => None,
    }
}

/// Prüft eine Farbeingabe; der Fehler nennt die Farbe (`label`) und die erwartete Form
pub fn check_color(label: &str, text: &str) -> Result<String, String> {
    normalize_hex(text).ok_or_else(|| {
        format!("Die Ringfarbe \"{label}\" ist ungültig: \"{}\" (erwartet #RRGGBB, z. B. #8aa8cc)", text.trim())
    })
}

fn rgb(hex: &str) -> Option<[u8; 3]> {
    let h = normalize_hex(hex)?;
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([byte(1)?, byte(3)?, byte(5)?])
}

/// Relative Leuchtdichte nach WCAG
fn luminance([r, g, b]: [u8; 3]) -> f64 {
    let lin = |v: u8| {
        let c = f64::from(v) / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// Kontrastverhältnis (1 bis 21) zweier Farben; `None`, wenn eine keine gültige Hex-Farbe ist
pub fn contrast_ratio(a: &str, b: &str) -> Option<f64> {
    let (la, lb) = (luminance(rgb(a)?), luminance(rgb(b)?));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    Some((hi + 0.05) / (lo + 0.05))
}

/// Ist die Ringfarbe gegen die Spur kaum sichtbar (Kontrast unter 1,5)? Ungültige Farben: nein.
pub fn hardly_visible(color: &str) -> bool {
    contrast_ratio(color, TRACK_COLOR).is_some_and(|r| r < MIN_RING_CONTRAST)
}

/// Die fünf Farben, wie sie Pille und Karte bekommen (alle als "#rrggbb")
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RingColors {
    pub five_hour: String,
    pub seven_day: String,
    pub context: String,
    pub warn: String,
    pub crit: String,
}

impl Default for RingColors {
    fn default() -> Self {
        Self::resolve(DEFAULT_FIVE_HOUR, DEFAULT_SEVEN_DAY, DEFAULT_CONTEXT, DEFAULT_WARN, DEFAULT_CRIT)
    }
}

impl RingColors {
    /// Aus den gespeicherten Texten. Eine unlesbare Farbe (z. B. von Hand in settings.json) wird durch ihren Standard ersetzt,
    /// damit die Fenster immer gültige Farben bekommen.
    pub fn resolve(five_hour: &str, seven_day: &str, context: &str, warn: &str, crit: &str) -> Self {
        let pick = |text: &str, default: &str| normalize_hex(text).unwrap_or_else(|| default.to_string());
        Self {
            five_hour: pick(five_hour, DEFAULT_FIVE_HOUR),
            seven_day: pick(seven_day, DEFAULT_SEVEN_DAY),
            context: pick(context, DEFAULT_CONTEXT),
            warn: pick(warn, DEFAULT_WARN),
            crit: pick(crit, DEFAULT_CRIT),
        }
    }
}

/// Ein Limit mit Reset (5 Stunden, Woche)
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// 0..=100
    pub used: f64,
    /// Epoch-Sekunden
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    /// 0..=100
    pub used: f64,
    pub size: Option<u64>,
    pub tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub updated_at: i64,
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub context: Option<Context>,
}

/// Sekunden, auch wenn die Quelle Millisekunden liefert (Werte über 1e12)
pub fn normalize_epoch(value: f64) -> i64 {
    if value > 1e12 { (value / 1000.0) as i64 } else { value as i64 }
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

fn count(v: &Value) -> Option<u64> {
    number(v).filter(|n| *n > 0.0).map(|n| n as u64)
}

fn window(v: &Value) -> Option<Window> {
    let used = number(v.get("used_percentage")?)?;
    let resets_at = v.get("resets_at").and_then(number).filter(|n| *n > 0.0).map(normalize_epoch);
    Some(Window { used: used.clamp(0.0, 100.0), resets_at })
}

fn context(v: &Value) -> Option<Context> {
    let used = number(v.get("used_percentage")?)?;
    Some(Context {
        used: used.clamp(0.0, 100.0),
        size: v.get("size").and_then(count),
        tokens: v.get("tokens").and_then(count),
    })
}

/// Liest die Datei tolerant: fehlende oder falsch typisierte Felder sind einfach leer. Nur ungültiges JSON
/// (oder ein anderer Wert als ein Objekt) ist ein Fehler. Fehlt `updated_at`, gilt `fallback_updated` (Änderungszeit der Datei).
pub fn parse(text: &str, fallback_updated: i64) -> Result<Limits, String> {
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}').trim()).map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("kein JSON-Objekt".to_string());
    }
    let updated_at = value.get("updated_at").and_then(number).map_or(fallback_updated, normalize_epoch);
    Ok(Limits {
        updated_at,
        five_hour: value.get("five_hour").and_then(window),
        seven_day: value.get("seven_day").and_then(window),
        context: value.get("context").and_then(context),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warn,
    Crit,
}

pub fn level(used: f64, t: Thresholds) -> Level {
    if used >= f64::from(t.crit) {
        Level::Crit
    } else if used >= f64::from(t.warn) {
        Level::Warn
    } else {
        Level::Ok
    }
}

/// Angezeigte Prozent, gerundet und auf 0..=100 geklemmt
pub fn display_percent(used: f64) -> u32 {
    used.clamp(0.0, 100.0).round() as u32
}

/// Das Limit zeigt 100 % (nach Rundung, also ab 99,5)
pub fn reached(used: f64) -> bool {
    display_percent(used) >= 100
}

/// `stroke-dasharray` für einen Ring mit Radius `radius`: gefüllter Anteil, dann eine volle Lücke
pub fn dasharray(used: f64, radius: f64) -> String {
    let circumference = 2.0 * std::f64::consts::PI * radius;
    let filled = circumference * used.clamp(0.0, 100.0) / 100.0;
    format!("{filled:.1} {circumference:.1}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    FiveHour,
    Week,
}

/// Ein Ring ist sichtbar, solange sein Reset in der Zukunft liegt (oder unbekannt ist)
fn active(w: &Window, now: i64) -> bool {
    w.resets_at.is_none_or(|r| r > now)
}

fn local(epoch: i64, offset: i32) -> NaiveDateTime {
    DateTime::from_timestamp(epoch + i64::from(offset), 0).map_or(DateTime::UNIX_EPOCH.naive_utc(), |d| d.naive_utc())
}

fn weekday_label(day: Weekday) -> &'static str {
    ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"][day.num_days_from_monday() as usize]
}

/// "14:32", "morgen 09:00", "Do 09:00", "15.10. 09:00". Mit `um` heisst heute "um 14:32", sonst "heute 14:32".
fn clock_label(now: i64, reset: i64, off_now: i32, off_reset: i32, um: bool) -> String {
    let (n, r) = (local(now, off_now), local(reset, off_reset));
    let time = r.format("%H:%M");
    match (r.date() - n.date()).num_days() {
        i64::MIN..=0 if um => format!("um {time}"),
        i64::MIN..=0 => format!("heute {time}"),
        1 => format!("morgen {time}"),
        2..=6 => format!("{} {time}", weekday_label(r.weekday())),
        _ => format!("{} {time}", r.format("%d.%m.")),
    }
}

/// "2 h 14 min", "14 min", "3 h" (aufgerundet auf volle Minuten)
fn duration_label(secs: i64) -> String {
    let minutes = (secs.max(0) + 59) / 60;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// Text unter dem Namen eines Limits.
/// Unter 100 %: 5 Stunden "Reset in 2 h 14 min", Woche "Reset Do 09:00" (heute/morgen wie unten).
/// Ab 100 %: "Limit erreicht · Reset um 14:32" (heute), "… Reset morgen 09:00", "… Reset Do 09:00" (Uhrzeit statt Restdauer).
pub fn reset_text(
    kind: Kind,
    used: f64,
    now: i64,
    resets_at: Option<i64>,
    off_now: i32,
    off_reset: i32,
) -> String {
    let full = reached(used);
    let Some(reset) = resets_at else {
        return if full { "Limit erreicht".to_string() } else { "Reset unbekannt".to_string() };
    };
    if full {
        return format!("Limit erreicht · Reset {}", clock_label(now, reset, off_now, off_reset, true));
    }
    let remaining = reset - now;
    if kind == Kind::FiveHour && remaining < 24 * 3600 {
        return format!("Reset in {}", duration_label(remaining));
    }
    format!("Reset {}", clock_label(now, reset, off_now, off_reset, false))
}

/// "122k", "2,5k", "850", "1,5M"
pub fn tokens_short(n: u64) -> String {
    let one_decimal = |value: f64, unit: &str| {
        let text = format!("{value:.1}");
        format!("{}{unit}", text.strip_suffix(".0").unwrap_or(&text).replace('.', ","))
    };
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => one_decimal(n as f64 / 1000.0, "k"),
        10_000..=999_499 => format!("{}k", (n as f64 / 1000.0).round() as u64),
        _ => one_decimal(n as f64 / 1_000_000.0, "M"),
    }
}

/// "122k von 200k Tokens"
pub fn context_text(ctx: &Context) -> String {
    let tokens = ctx.tokens.or_else(|| ctx.size.map(|s| (s as f64 * ctx.used / 100.0).round() as u64));
    match (tokens, ctx.size) {
        (Some(t), Some(s)) => format!("{} von {} Tokens", tokens_short(t), tokens_short(s)),
        (Some(t), None) => format!("{} Tokens", tokens_short(t)),
        (None, Some(s)) => format!("Fenster {} Tokens", tokens_short(s)),
        (None, None) => "Füllstand des Kontexts".to_string(),
    }
}

/// "Stand vor 20 s"
pub fn age_text(secs: i64) -> String {
    match secs.max(0) {
        0..=2 => "Stand gerade eben".to_string(),
        s @ 3..=59 => format!("Stand vor {s} s"),
        s @ 60..=3599 => format!("Stand vor {} min", s / 60),
        s => format!("Stand vor {} h", s / 3600),
    }
}

/// Ein Ring, wie ihn Pille und Karte zeichnen. Fehlt `pct`, gibt es keine (gültigen) Daten.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ring {
    pub key: &'static str,
    pub label: &'static str,
    pub pct: Option<u32>,
    pub level: Level,
    pub reached: bool,
    pub dash_pill: String,
    pub dash_card: String,
    pub detail: String,
}

impl Ring {
    fn empty(key: &'static str, label: &'static str, detail: &str) -> Self {
        Self {
            key,
            label,
            pct: None,
            level: Level::Ok,
            reached: false,
            dash_pill: String::new(),
            dash_card: String::new(),
            detail: detail.to_string(),
        }
    }

    fn filled(key: &'static str, label: &'static str, used: f64, ring: usize, detail: String, t: Thresholds) -> Self {
        Self {
            key,
            label,
            pct: Some(display_percent(used)),
            level: level(used, t),
            reached: reached(used),
            dash_pill: dasharray(used, PILL_RADII[ring]),
            dash_card: dasharray(used, CARD_RADIUS),
            detail,
        }
    }
}

/// Was angezeigt wird: immer drei Ringe (aussen 5 Stunden, Mitte Woche, innen Kontext), fehlende sind leer
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct View {
    pub rings: [Ring; 3],
    pub aria: String,
    pub updated_at: i64,
}

fn aria_part(ring: &Ring, name: &str) -> String {
    match ring.pct {
        Some(p) => format!("{name} {p} Prozent"),
        None => format!("{name} keine Daten"),
    }
}

impl Limits {
    /// `offset(epoch)`: Sekunden Unterschied der lokalen Zeit zu UTC zu diesem Zeitpunkt.
    /// `None`, wenn die Datei zu alt ist oder kein Ring brauchbare Daten hat.
    pub fn view(&self, now: i64, offset: &dyn Fn(i64) -> i32, t: Thresholds) -> Option<View> {
        if now - self.updated_at > STALE_AFTER_SECS {
            return None;
        }
        let off_now = offset(now);
        let timed = |key, label, kind, w: &Option<Window>, ring| match w.as_ref().filter(|w| active(w, now)) {
            Some(w) => {
                let off_reset = w.resets_at.map_or(off_now, offset);
                let detail = reset_text(kind, w.used, now, w.resets_at, off_now, off_reset);
                Ring::filled(key, label, w.used, ring, detail, t)
            }
            None if w.is_some() => Ring::empty(key, label, "Reset vorbei, wartet auf neue Daten"),
            None => Ring::empty(key, label, "Kein Limit gemeldet"),
        };
        let five = timed("five_hour", "5-Stunden-Limit", Kind::FiveHour, &self.five_hour, 0);
        let week = timed("seven_day", "Wochenlimit", Kind::Week, &self.seven_day, 1);
        let ctx = match &self.context {
            Some(c) => Ring::filled("context", "Kontext", c.used, 2, context_text(c), t),
            None => Ring::empty("context", "Kontext", "Kein Kontext gemeldet"),
        };
        if five.pct.is_none() && week.pct.is_none() && ctx.pct.is_none() {
            return None;
        }
        let aria = format!(
            "Claude-Limits anzeigen: {}, {}, {}",
            aria_part(&five, "5 Stunden"),
            aria_part(&week, "Woche"),
            aria_part(&ctx, "Kontext"),
        );
        Some(View { rings: [five, week, ctx], aria, updated_at: self.updated_at })
    }
}

/// Was die Pille bekommt: Ringe (oder `null`) und die Farben, im Feld `limits` und `limits_colors` des Pillen-Zustands
pub fn add_to_pill_data(data: &mut Value, view: &Option<View>, colors: &RingColors) {
    data["limits"] = serde_json::json!(view);
    data["limits_colors"] = serde_json::json!(colors);
}

/// Was die Detailkarte bekommt: Ringe, Alter und Farben (`null`, wenn es keine Daten gibt)
pub fn card_data(view: &Option<View>, colors: &RingColors, now: i64) -> Value {
    match view {
        Some(v) => serde_json::json!({ "rings": v.rings, "age": age_text(now - v.updated_at), "colors": colors }),
        None => Value::Null,
    }
}

/// Offset der lokalen Zeit (Sekunden) zu einem Zeitpunkt, mit Sommerzeit
pub fn local_offset(epoch: i64) -> i32 {
    Local.timestamp_opt(epoch, 0).single().map_or(0, |d| d.offset().local_minus_utc())
}

/// Beobachtet die Datei: liest sie nur neu, wenn sich Änderungszeit oder Grösse ändern
pub struct LimitsWatcher {
    path: PathBuf,
    seen: Option<(SystemTime, u64)>,
    current: Option<Limits>,
    /// Zuletzt geloggter Fehler: derselbe Fehler wird nicht bei jedem Poll wiederholt
    logged: Option<String>,
}

impl LimitsWatcher {
    pub fn new(path: PathBuf) -> Self {
        Self { path, seen: None, current: None, logged: None }
    }

    pub fn current(&self) -> Option<&Limits> {
        self.current.as_ref()
    }

    fn complain(&mut self, msg: String) {
        if self.logged.as_ref() != Some(&msg) {
            log::warn(&msg);
            self.logged = Some(msg);
        }
    }

    /// Schaut nach der Datei. Gibt `true` zurück, wenn sich der Inhalt geändert hat.
    pub fn poll(&mut self) -> bool {
        let before = self.current.clone();
        match std::fs::metadata(&self.path) {
            Err(e) => {
                self.seen = None;
                self.current = None;
                if e.kind() != std::io::ErrorKind::NotFound {
                    self.complain(format!("claude-limits.json nicht lesbar: {e}"));
                }
            }
            Ok(meta) => {
                let stamp = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
                if self.seen != Some(stamp) {
                    self.seen = Some(stamp);
                    let fallback = stamp
                        .0
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs() as i64);
                    match std::fs::read_to_string(&self.path) {
                        Ok(text) => match parse(&text, fallback) {
                            Ok(limits) => {
                                self.current = Some(limits);
                                self.logged = None;
                            }
                            Err(e) => {
                                self.current = None;
                                self.complain(format!("claude-limits.json ungültig, keine Daten: {e}"));
                            }
                        },
                        Err(e) => {
                            self.seen = None;
                            self.current = None;
                            self.complain(format!("claude-limits.json nicht lesbar: {e}"));
                        }
                    }
                }
            }
        }
        self.current != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_791_000_000; // 2026-10-03 03:20:00 UTC

    fn at(utc: &str) -> i64 {
        chrono::NaiveDateTime::parse_from_str(utc, "%Y-%m-%d %H:%M").unwrap().and_utc().timestamp()
    }

    fn off(seconds: i32) -> impl Fn(i64) -> i32 {
        move |_| seconds
    }

    const FULL: &str = r#"{"updated_at":1791000000,
        "five_hour":{"used_percentage":42.5,"resets_at":1791009000},
        "seven_day":{"used_percentage":27,"resets_at":1791300000},
        "context":{"used_percentage":61.2,"size":200000,"tokens":122400}}"#;

    #[test]
    fn parses_a_complete_file() {
        let l = parse(FULL, 5).unwrap();
        assert_eq!(l.updated_at, NOW);
        assert_eq!(l.five_hour, Some(Window { used: 42.5, resets_at: Some(1_791_009_000) }));
        assert_eq!(l.seven_day.unwrap().used, 27.0);
        assert_eq!(l.context, Some(Context { used: 61.2, size: Some(200_000), tokens: Some(122_400) }));
    }

    #[test]
    fn null_and_missing_fields_are_empty() {
        let l = parse(r#"{"updated_at":10,"five_hour":null,"seven_day":null,"context":null}"#, 5).unwrap();
        assert_eq!((l.five_hour, l.seven_day, l.context), (None, None, None));
        let l = parse(r#"{"updated_at":10}"#, 5).unwrap();
        assert_eq!((l.five_hour, l.seven_day, l.context), (None, None, None));
        // ohne updated_at gilt die Änderungszeit der Datei
        assert_eq!(parse("{}", 77).unwrap().updated_at, 77);
        // Fenster ohne Reset, Kontext ohne Grösse, falsche Typen
        let l = parse(
            r#"{"updated_at":10,"five_hour":{"used_percentage":50},"seven_day":{"used_percentage":"viel","resets_at":5},
                "context":{"used_percentage":20,"size":"gross","tokens":null}}"#,
            0,
        )
        .unwrap();
        assert_eq!(l.five_hour, Some(Window { used: 50.0, resets_at: None }));
        assert_eq!(l.seven_day, None);
        assert_eq!(l.context, Some(Context { used: 20.0, size: None, tokens: None }));
        // ein Fenster ohne Prozent ist leer
        assert_eq!(parse(r#"{"five_hour":{"resets_at":5}}"#, 0).unwrap().five_hour, None);
    }

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        for bad in ["", "   ", "{", r#"{"updated_at":17"#, "null", "[]", "42", "\"x\"", "nicht json", "\u{0}\u{0}"] {
            assert!(parse(bad, 0).is_err(), "{bad:?}");
        }
        // BOM und Leerraum stören nicht
        assert!(parse("\u{feff}\n {\"updated_at\":1}\n", 0).is_ok());
    }

    #[test]
    fn percentages_are_clamped() {
        let l = parse(r#"{"updated_at":1,"five_hour":{"used_percentage":130,"resets_at":9},"seven_day":{"used_percentage":-4}}"#, 0)
            .unwrap();
        assert_eq!(l.five_hour.unwrap().used, 100.0);
        assert_eq!(l.seven_day.unwrap().used, 0.0);
        assert_eq!(display_percent(250.0), 100);
        assert_eq!(display_percent(-3.0), 0);
        assert_eq!(display_percent(42.5), 43);
        assert_eq!(display_percent(42.4), 42);
    }

    #[test]
    fn milliseconds_are_accepted() {
        let l = parse(
            r#"{"updated_at":1791000000000,"five_hour":{"used_percentage":10,"resets_at":1791009000000}}"#,
            0,
        )
        .unwrap();
        assert_eq!(l.updated_at, NOW);
        assert_eq!(l.five_hour.unwrap().resets_at, Some(1_791_009_000));
        assert_eq!(normalize_epoch(1.5e12), 1_500_000_000);
        assert_eq!(normalize_epoch(1.0e12), 1_000_000_000_000, "genau 1e12 bleibt Sekunden");
        assert_eq!(normalize_epoch(1_791_000_000.0), NOW);
    }

    #[test]
    fn warning_levels() {
        assert_eq!(level(0.0, Thresholds::default()), Level::Ok);
        assert_eq!(level(79.9, Thresholds::default()), Level::Ok);
        assert_eq!(level(80.0, Thresholds::default()), Level::Warn);
        assert_eq!(level(94.9, Thresholds::default()), Level::Warn);
        assert_eq!(level(95.0, Thresholds::default()), Level::Crit);
        assert_eq!(level(100.0, Thresholds::default()), Level::Crit);
        assert!(!reached(99.4) && reached(99.5) && reached(100.0));
    }

    #[test]
    fn custom_thresholds_move_the_levels() {
        let t = Thresholds::check(50, 60).unwrap();
        assert_eq!(level(49.9, t), Level::Ok);
        assert_eq!(level(50.0, t), Level::Warn);
        assert_eq!(level(60.0, t), Level::Crit);
        let v = parse(FULL, 0).unwrap().view(NOW, &off(0), t).unwrap();
        let strict = parse(FULL, 0).unwrap().view(NOW, &off(0), Thresholds::check(1, 2).unwrap()).unwrap();
        assert!(strict.rings.iter().filter(|r| r.pct.is_some()).all(|r| r.level == Level::Crit));
        assert_ne!(v, strict);
    }

    #[test]
    fn thresholds_are_validated() {
        assert_eq!(Thresholds::check(1, 100), Ok(Thresholds { warn: 1, crit: 100 }));
        assert!(Thresholds::check(0, 50).is_err());
        assert!(Thresholds::check(50, 101).is_err());
        assert!(Thresholds::check(95, 95).is_err());
        assert!(Thresholds::check(96, 95).is_err());
        assert_eq!(Thresholds::resolve(96, 95), Thresholds::default());
        assert_eq!(Thresholds::resolve(10, 20), Thresholds { warn: 10, crit: 20 });
    }

    #[test]
    fn percent_to_dasharray() {
        // Werte des Entwurfs: 42 % auf dem äusseren Ring, 27 % in der Mitte, 61 % innen
        assert_eq!(dasharray(42.0, 19.5), "51.5 122.5"); // der Entwurf rundet 51,46 auf 51.4
        assert_eq!(dasharray(27.0, 14.0), "23.8 88.0");
        assert_eq!(dasharray(61.0, 8.5), "32.6 53.4");
        assert_eq!(dasharray(0.0, 16.0), "0.0 100.5");
        assert_eq!(dasharray(100.0, 16.0), "100.5 100.5");
        assert_eq!(dasharray(180.0, 16.0), "100.5 100.5", "geklemmt");
        assert_eq!(dasharray(-5.0, 16.0), "0.0 100.5");
    }

    #[test]
    fn stale_file_means_no_data() {
        let l = parse(FULL, 0).unwrap();
        let o = off(0);
        assert!(l.view(NOW + 30, &o, Thresholds::default()).is_some());
        assert!(l.view(NOW + STALE_AFTER_SECS, &o, Thresholds::default()).is_some(), "genau 6 h ist noch frisch");
        // das Fenster-Ende liegt davor: Rest ist 5 h 49 min nach NOW, also hier nur die Frische prüfen
        let fresh_reset = Limits {
            five_hour: Some(Window { used: 10.0, resets_at: Some(NOW + 10 * 3600) }),
            ..l.clone()
        };
        assert!(fresh_reset.view(NOW + STALE_AFTER_SECS + 1, &o, Thresholds::default()).is_none());
        // eine Uhr, die vorgeht, macht die Datei nicht alt
        assert!(l.view(NOW - 600, &o, Thresholds::default()).is_some());
    }

    #[test]
    fn expired_reset_hides_only_that_ring() {
        let l = parse(FULL, 0).unwrap();
        let o = off(0);
        // 5-h-Reset war NOW+9000: danach ist der Ring weg, Woche und Kontext bleiben
        let v = l.view(NOW + 9001, &o, Thresholds::default()).unwrap();
        assert_eq!(v.rings[0].pct, None);
        assert_eq!(v.rings[0].dash_pill, "");
        assert_eq!(v.rings[1].pct, Some(27));
        assert_eq!(v.rings[2].pct, Some(61));
        assert!(v.aria.contains("5 Stunden keine Daten") && v.aria.contains("Woche 27 Prozent"));
        // genau zum Reset ist er auch weg
        assert_eq!(l.view(NOW + 9000, &o, Thresholds::default()).unwrap().rings[0].pct, None);
        assert_eq!(l.view(NOW + 8999, &o, Thresholds::default()).unwrap().rings[0].pct, Some(43));
    }

    #[test]
    fn no_usable_ring_means_no_view() {
        let o = off(0);
        let only_expired = Limits {
            updated_at: NOW,
            five_hour: Some(Window { used: 99.0, resets_at: Some(NOW - 1) }),
            seven_day: None,
            context: None,
        };
        assert!(only_expired.view(NOW, &o, Thresholds::default()).is_none());
        let nothing = Limits { updated_at: NOW, five_hour: None, seven_day: None, context: None };
        assert!(nothing.view(NOW, &o, Thresholds::default()).is_none());
        // Kontext allein genügt, solange die Datei frisch ist
        let ctx = Limits { context: Some(Context { used: 5.0, size: None, tokens: None }), ..nothing };
        let v = ctx.view(NOW + 3 * 3600, &o, Thresholds::default()).unwrap();
        assert_eq!((v.rings[0].pct, v.rings[1].pct, v.rings[2].pct), (None, None, Some(5)));
        assert!(ctx.view(NOW + 7 * 3600, &o, Thresholds::default()).is_none());
    }

    #[test]
    fn view_of_a_full_file() {
        let v = parse(FULL, 0).unwrap().view(NOW, &off(0), Thresholds::default()).unwrap();
        let [five, week, ctx] = &v.rings;
        assert_eq!((five.pct, week.pct, ctx.pct), (Some(43), Some(27), Some(61)));
        assert_eq!(five.detail, "Reset in 2 h 30 min");
        assert_eq!(ctx.detail, "122k von 200k Tokens");
        assert_eq!((five.level, week.level, ctx.level), (Level::Ok, Level::Ok, Level::Ok));
        assert_eq!(
            v.aria,
            "Claude-Limits anzeigen: 5 Stunden 43 Prozent, Woche 27 Prozent, Kontext 61 Prozent"
        );
        assert_eq!(five.dash_pill, dasharray(42.5, 19.5));
        assert_eq!(five.dash_card, dasharray(42.5, 16.0));
    }

    #[test]
    fn warnings_apply_per_ring() {
        let l = Limits {
            updated_at: NOW,
            five_hour: Some(Window { used: 82.0, resets_at: Some(NOW + 600) }),
            seven_day: Some(Window { used: 96.0, resets_at: Some(NOW + 86_400 * 2) }),
            context: Some(Context { used: 10.0, size: Some(1_000_000), tokens: Some(100_000) }),
        };
        let v = l.view(NOW, &off(0), Thresholds::default()).unwrap();
        assert_eq!([v.rings[0].level, v.rings[1].level, v.rings[2].level], [Level::Warn, Level::Crit, Level::Ok]);
    }

    #[test]
    fn five_hour_reset_text_below_100() {
        let t = |secs| reset_text(Kind::FiveHour, 42.0, NOW, Some(NOW + secs), 0, 0);
        assert_eq!(t(2 * 3600 + 14 * 60), "Reset in 2 h 14 min");
        assert_eq!(t(14 * 60), "Reset in 14 min");
        assert_eq!(t(3 * 3600), "Reset in 3 h");
        assert_eq!(t(61), "Reset in 2 min", "aufgerundet");
        assert_eq!(t(5), "Reset in 1 min");
        assert_eq!(reset_text(Kind::FiveHour, 42.0, NOW, None, 0, 0), "Reset unbekannt");
    }

    #[test]
    fn week_reset_text_below_100() {
        let now = at("2026-10-05 08:00"); // Montag
        let t = |reset: &str| reset_text(Kind::Week, 27.0, now, Some(at(reset)), 0, 0);
        assert_eq!(t("2026-10-08 09:00"), "Reset Do 09:00");
        assert_eq!(t("2026-10-05 22:15"), "Reset heute 22:15");
        assert_eq!(t("2026-10-06 00:05"), "Reset morgen 00:05");
        assert_eq!(t("2026-10-11 23:59"), "Reset So 23:59");
        assert_eq!(t("2026-10-12 09:00"), "Reset 12.10. 09:00", "ab einer Woche mit Datum");
    }

    #[test]
    fn full_limit_shows_the_clock_time() {
        let now = at("2026-10-05 08:00");
        let t = |kind, used, reset: &str| reset_text(kind, used, now, Some(at(reset)), 0, 0);
        assert_eq!(t(Kind::FiveHour, 100.0, "2026-10-05 14:32"), "Limit erreicht · Reset um 14:32");
        assert_eq!(t(Kind::FiveHour, 99.5, "2026-10-05 14:32"), "Limit erreicht · Reset um 14:32");
        assert_eq!(t(Kind::FiveHour, 99.4, "2026-10-05 14:32"), "Reset in 6 h 32 min");
        assert_eq!(t(Kind::FiveHour, 100.0, "2026-10-06 01:10"), "Limit erreicht · Reset morgen 01:10");
        assert_eq!(t(Kind::Week, 100.0, "2026-10-06 09:00"), "Limit erreicht · Reset morgen 09:00");
        assert_eq!(t(Kind::Week, 100.0, "2026-10-08 09:00"), "Limit erreicht · Reset Do 09:00");
        assert_eq!(t(Kind::Week, 100.0, "2026-10-05 17:00"), "Limit erreicht · Reset um 17:00");
        assert_eq!(reset_text(Kind::Week, 100.0, now, None, 0, 0), "Limit erreicht");
    }

    #[test]
    fn reset_text_uses_the_local_time_zone() {
        // 22:30 UTC: in Zürich (MESZ, +2 h) schon 00:30 am nächsten Tag
        let now = at("2026-10-05 20:00");
        let reset = at("2026-10-05 22:30");
        let utc = reset_text(Kind::Week, 100.0, now, Some(reset), 0, 0);
        let zurich = reset_text(Kind::Week, 100.0, now, Some(reset), 7200, 7200);
        let new_york = reset_text(Kind::Week, 100.0, now, Some(reset), -4 * 3600, -4 * 3600);
        assert_eq!(utc, "Limit erreicht · Reset um 22:30");
        assert_eq!(zurich, "Limit erreicht · Reset morgen 00:30");
        assert_eq!(new_york, "Limit erreicht · Reset um 18:30");
        // Sommerzeitwechsel zwischen jetzt und Reset: der Offset des Resets zählt
        let now = at("2026-10-22 08:00");
        let reset = at("2026-10-26 08:00"); // 09:00 MEZ
        assert_eq!(reset_text(Kind::Week, 10.0, now, Some(reset), 7200, 3600), "Reset Mo 09:00");
        // 5-h-Reset über Mitternacht: relativ, ohne Zonenrechnung
        assert_eq!(reset_text(Kind::FiveHour, 10.0, now, Some(now + 7200), 7200, 7200), "Reset in 2 h");
    }

    #[test]
    fn view_uses_the_offset_of_each_instant() {
        let now = at("2026-10-24 12:00");
        let reset = at("2026-10-26 08:00");
        let l = Limits {
            updated_at: now,
            five_hour: None,
            seven_day: Some(Window { used: 10.0, resets_at: Some(reset) }),
            context: None,
        };
        // Sommerzeit endet in der Nacht auf den 25.10. 01:00 UTC
        let zurich = |t: i64| if t >= at("2026-10-25 01:00") { 3600 } else { 7200 };
        let v = l.view(now, &zurich, Thresholds::default()).unwrap();
        assert_eq!(v.rings[1].detail, "Reset Mo 09:00");
    }

    #[test]
    fn token_short_form() {
        for (n, s) in [
            (0, "0"),
            (850, "850"),
            (999, "999"),
            (1000, "1k"),
            (2500, "2,5k"),
            (9999, "10k"),
            (10_000, "10k"),
            (122_400, "122k"),
            (200_000, "200k"),
            (999_400, "999k"),
            (1_000_000, "1M"),
            (1_500_000, "1,5M"),
        ] {
            assert_eq!(tokens_short(n), s, "{n}");
        }
    }

    #[test]
    fn context_text_variants() {
        let c = |used, size, tokens| Context { used, size, tokens };
        assert_eq!(context_text(&c(61.2, Some(200_000), Some(122_400))), "122k von 200k Tokens");
        assert_eq!(context_text(&c(50.0, Some(200_000), None)), "100k von 200k Tokens");
        assert_eq!(context_text(&c(50.0, None, Some(3_000))), "3k Tokens");
        assert_eq!(context_text(&c(50.0, None, None)), "Füllstand des Kontexts");
    }

    #[test]
    fn age_text_steps() {
        assert_eq!(age_text(-10), "Stand gerade eben");
        assert_eq!(age_text(2), "Stand gerade eben");
        assert_eq!(age_text(20), "Stand vor 20 s");
        assert_eq!(age_text(59), "Stand vor 59 s");
        assert_eq!(age_text(60), "Stand vor 1 min");
        assert_eq!(age_text(3599), "Stand vor 59 min");
        assert_eq!(age_text(3 * 3600 + 5), "Stand vor 3 h");
    }

    #[test]
    fn weekday_labels() {
        assert_eq!(weekday_label(Weekday::Mon), "Mo");
        assert_eq!(weekday_label(Weekday::Thu), "Do");
        assert_eq!(weekday_label(Weekday::Sun), "So");
    }

    #[test]
    fn hex_colors_are_normalized() {
        for (input, want) in [
            ("#ededed", "#ededed"),
            ("#EDEDED", "#ededed"),
            ("ededed", "#ededed"),
            ("  #8AA8CC  ", "#8aa8cc"),
            ("#fff", "#ffffff"),
            ("#AbC", "#aabbcc"),
            ("abc", "#aabbcc"),
            ("000", "#000000"),
        ] {
            assert_eq!(normalize_hex(input).as_deref(), Some(want), "{input:?}");
        }
        for bad in [
            "", "#", "##fff", "#ff", "#ffff", "#fffff", "#fffffff", "#ggg", "#12345z", "rot", "rgb(1,2,3)", "# fff",
            "#ff ff", "0x123456", "#ä12", "#１２３", "\u{0}", "#fff\n#000", "fff;", "#fff'", "<b>",
        ] {
            assert_eq!(normalize_hex(bad), None, "{bad:?}");
        }
        // Fehlertext nennt die Farbe und die Eingabe
        let err = check_color("5 Stunden", " #xyz ").unwrap_err();
        assert!(err.contains("5 Stunden") && err.contains("#xyz") && err.contains("#RRGGBB"), "{err}");
        assert_eq!(check_color("x", "FFF").unwrap(), "#ffffff");
    }

    #[test]
    fn contrast_against_the_track() {
        assert!((contrast_ratio("#000000", "#ffffff").unwrap() - 21.0).abs() < 1e-9);
        assert!((contrast_ratio("#fff", "#FFFFFF").unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(contrast_ratio("rot", "#fff"), None);
        // symmetrisch
        assert_eq!(contrast_ratio("#123456", "#abcdef"), contrast_ratio("#abcdef", "#123456"));
        // alle Standardfarben und Presets sind gut sichtbar
        let d = RingColors::default();
        for c in [&d.five_hour, &d.seven_day, &d.context, &d.warn, &d.crit] {
            assert!(!hardly_visible(c), "{c}");
        }
        for c in ["#8aa8cc", "#a99bc7", "#c2a27e", "#84b3a6"] {
            assert!(!hardly_visible(c), "{c}");
        }
        // zu dunkel: Spur selbst, Schwarz, fast gleiche Töne
        for c in [TRACK_COLOR, "#000000", "#111", "#222222", "#1f1f1f"] {
            assert!(hardly_visible(c), "{c}");
        }
        // Grenze: der erste Grauton mit Verhältnis >= 1,5 ist sichtbar
        let ratio = |v: u8| contrast_ratio(&format!("#{v:02x}{v:02x}{v:02x}"), TRACK_COLOR).unwrap();
        let first_ok = (0..=255u8).find(|&v| ratio(v) >= MIN_RING_CONTRAST).unwrap();
        assert!(hardly_visible(&format!("#{0:02x}{0:02x}{0:02x}", first_ok - 1)));
        assert!(!hardly_visible(&format!("#{0:02x}{0:02x}{0:02x}", first_ok)));
        // ungültig: kein Hinweis
        assert!(!hardly_visible("kaputt"));
    }

    #[test]
    fn colors_reach_pill_and_card() {
        let colors = RingColors::resolve("#8aa8cc", "#a99bc7", "#c2a27e", "#84b3a6", "#ff0000");
        let view = parse(FULL, 0).unwrap().view(NOW, &off(0), Thresholds::default());
        assert!(view.is_some());

        let mut pill = serde_json::json!({ "short": "Do" });
        add_to_pill_data(&mut pill, &view, &colors);
        assert_eq!(pill["short"], "Do", "andere Felder bleiben");
        assert_eq!(pill["limits_colors"]["five_hour"], "#8aa8cc");
        assert_eq!(pill["limits_colors"]["seven_day"], "#a99bc7");
        assert_eq!(pill["limits_colors"]["context"], "#c2a27e");
        assert_eq!(pill["limits_colors"]["warn"], "#84b3a6");
        assert_eq!(pill["limits_colors"]["crit"], "#ff0000");
        assert_eq!(pill["limits"]["rings"][0]["key"], "five_hour");

        let card = card_data(&view, &colors, NOW + 20);
        assert_eq!(card["colors"], pill["limits_colors"]);
        assert_eq!(card["age"], "Stand vor 20 s");
        assert_eq!(card["rings"].as_array().unwrap().len(), 3);

        // ohne Daten: die Pille bekommt trotzdem die Farben (der Ring ist dann nur ausgeblendet), die Karte null
        let mut none = serde_json::json!({});
        add_to_pill_data(&mut none, &None, &colors);
        assert!(none["limits"].is_null());
        assert_eq!(none["limits_colors"]["crit"], "#ff0000");
        assert!(card_data(&None, &colors, NOW).is_null());
    }

    #[test]
    fn ring_colors_resolve_with_fallbacks() {
        let d = RingColors::default();
        assert_eq!(
            (d.five_hour.as_str(), d.seven_day.as_str(), d.context.as_str(), d.warn.as_str(), d.crit.as_str()),
            ("#ededed", "#8a8a8a", "#5c5c5c", "#d6b878", "#d28f8f")
        );
        let c = RingColors::resolve("ABC", "8AA8CC", "kaputt", "", "#D28F8F");
        assert_eq!(c.five_hour, "#aabbcc");
        assert_eq!(c.seven_day, "#8aa8cc");
        assert_eq!(c.context, DEFAULT_CONTEXT, "unlesbar: Standard");
        assert_eq!(c.warn, DEFAULT_WARN);
        assert_eq!(c.crit, "#d28f8f");
    }

    #[test]
    fn watcher_reads_on_change_and_tolerates_trouble() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("claude-limits.json");
        let mut w = LimitsWatcher::new(file.clone());
        // keine Datei
        assert!(!w.poll());
        assert!(w.current().is_none());
        // gültige Datei
        std::fs::write(&file, FULL).unwrap();
        assert!(w.poll());
        assert_eq!(w.current().unwrap().updated_at, NOW);
        // unverändert: kein neuer Inhalt
        assert!(!w.poll());
        // halb geschrieben: keine Daten, kein Absturz; derselbe Fehler wird nur einmal geloggt
        std::fs::write(&file, &FULL[..40]).unwrap();
        assert!(w.poll());
        assert!(w.current().is_none());
        let logged = w.logged.clone();
        assert!(logged.is_some());
        assert!(!w.poll());
        assert_eq!(w.logged, logged);
        // repariert
        std::fs::write(&file, FULL).unwrap();
        assert!(w.poll());
        assert!(w.current().is_some() && w.logged.is_none());
        // gelöscht
        std::fs::remove_file(&file).unwrap();
        assert!(w.poll());
        assert!(w.current().is_none());
    }
}
