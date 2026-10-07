use crate::export::{WeekData, german_weekday};

const PAGE_WIDTH: u32 = 11906;
const PAGE_HEIGHT: u32 = 16838;
const MARGIN: u32 = 1134;
const COLUMNS: [u32; 4] = [2000, 4638, 1500, 1500];

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

const DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:cs="Calibri"/><w:sz w:val="22"/><w:szCs w:val="22"/><w:lang w:val="de-CH"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="0" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="240" w:after="120"/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/><w:szCs w:val="32"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before="240" w:after="80"/><w:outlineLvl w:val="1"/></w:pPr><w:rPr><w:b/><w:sz w:val="26"/><w:szCs w:val="26"/></w:rPr></w:style></w:styles>"#;

/// Entfernt Zeichen, die in XML 1.0 nicht erlaubt sind, und schützt `& < > "`
pub fn xml_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || matches!(c as u32, 0xFFFE | 0xFFFF) => {}
            c => out.push(c),
        }
    }
    out
}

fn paragraph(text: &str, style: Option<&str>, bold: bool) -> String {
    let props = style.map_or_else(String::new, |s| format!(r#"<w:pPr><w:pStyle w:val="{s}"/></w:pPr>"#));
    if text.is_empty() {
        return format!("<w:p>{props}</w:p>");
    }
    let run_props = if bold { "<w:rPr><w:b/></w:rPr>" } else { "" };
    format!(
        r#"<w:p>{props}<w:r>{run_props}<w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
        xml_text(text)
    )
}

/// Mehrzeiliger Text als eine Zeile pro Absatz. Leerer Text wird zu einem Strich.
fn text_block(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        return paragraph("–", None, false);
    }
    lines.iter().map(|l| paragraph(l, None, false)).collect()
}

fn cell(width: u32, paragraphs: Vec<String>, shaded: bool) -> String {
    let shading = if shaded { r#"<w:shd w:val="clear" w:color="auto" w:fill="D9E2F3"/>"# } else { "" };
    let body = if paragraphs.is_empty() { paragraph("", None, false) } else { paragraphs.concat() };
    format!(r#"<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/>{shading}</w:tcPr>{body}</w:tc>"#)
}

fn table(week: &WeekData) -> String {
    let border = |side: &str| format!(r#"<w:{side} w:val="single" w:sz="4" w:space="0" w:color="000000"/>"#);
    let borders: String = ["top", "left", "bottom", "right", "insideH", "insideV"].iter().map(|s| border(s)).collect();
    let mut xml = format!(
        r#"<w:tbl><w:tblPr><w:tblW w:w="{}" w:type="dxa"/><w:tblBorders>{borders}</w:tblBorders><w:tblLayout w:type="fixed"/><w:tblCellMar><w:top w:w="40" w:type="dxa"/><w:left w:w="100" w:type="dxa"/><w:bottom w:w="40" w:type="dxa"/><w:right w:w="100" w:type="dxa"/></w:tblCellMar></w:tblPr><w:tblGrid>{}</w:tblGrid>"#,
        COLUMNS.iter().sum::<u32>(),
        COLUMNS.iter().map(|w| format!(r#"<w:gridCol w:w="{w}"/>"#)).collect::<String>(),
    );

    let header = ["Tag (wo und wann)", "Tätigkeit", "Zeit", "Zeit (Total)"];
    xml.push_str("<w:tr><w:trPr><w:tblHeader/></w:trPr>");
    for (title, width) in header.iter().zip(COLUMNS) {
        xml.push_str(&cell(width, vec![paragraph(title, None, true)], true));
    }
    xml.push_str("</w:tr>");

    for day in &week.days {
        let date = day.date.format("%d.%m.%y").to_string();
        let day_cell = vec![
            paragraph(german_weekday(day.date), None, true),
            paragraph(&date, None, false),
            paragraph(&day.place, None, false),
        ];
        let activities = day.activities.iter().map(|a| paragraph(a, None, false)).collect();
        let times = day.times.iter().map(|t| paragraph(t, None, false)).collect();
        xml.push_str("<w:tr>");
        xml.push_str(&cell(COLUMNS[0], day_cell, false));
        xml.push_str(&cell(COLUMNS[1], activities, false));
        xml.push_str(&cell(COLUMNS[2], times, false));
        xml.push_str(&cell(COLUMNS[3], vec![paragraph(&day.total, None, false)], false));
        xml.push_str("</w:tr>");
    }
    xml.push_str("</w:tbl>");
    xml
}

pub fn document_xml(week: &WeekData) -> String {
    let fmt = |d: chrono::NaiveDate| d.format("%d.%m.%y").to_string();
    let mut body = String::new();
    body.push_str(&paragraph(&format!("Arbeitsjournal KW {} {}", week.week, week.year), Some("Heading1"), false));
    body.push_str(&paragraph(&format!("{} – {}", fmt(week.from), fmt(week.to)), None, false));
    body.push_str(&paragraph("Tätigkeitstabelle", Some("Heading2"), false));
    body.push_str(&table(week));
    body.push_str(&paragraph("", None, false));
    for (title, text) in [
        ("Wochenrückblick", &week.review),
        ("Reflexion", &week.reflection),
        ("Stimmung der Woche", &week.mood),
    ] {
        body.push_str(&paragraph(title, Some("Heading2"), false));
        body.push_str(&text_block(text));
    }
    body.push_str(&paragraph("Tabellenverzeichnis", Some("Heading2"), false));
    body.push_str(&paragraph("Tabelle 1 – Tätigkeitstabelle", None, false));
    body.push_str(&paragraph("", None, false));
    body.push_str(&paragraph("Datum: ______________________        Unterschrift: ______________________", None, false));

    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr><w:pgSz w:w="{PAGE_WIDTH}" w:h="{PAGE_HEIGHT}"/><w:pgMar w:top="{MARGIN}" w:right="{MARGIN}" w:bottom="{MARGIN}" w:left="{MARGIN}" w:header="709" w:footer="709" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    )
}

/// Die fertige .docx-Datei als Bytes
pub fn week_to_docx(week: &WeekData) -> Vec<u8> {
    let document = document_xml(week);
    zip_stored(&[
        ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", DOCUMENT_RELS.as_bytes()),
        ("word/styles.xml", STYLES.as_bytes()),
    ])
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Minimales ZIP ohne Kompression (Methode "stored"). Für ein kleines .docx reicht das.
pub fn zip_stored(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    // Zeit 00:00:00, Datum 01.01.1980
    let (dos_time, dos_date) = (0u16, 0x0021u16);

    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        let name = name.as_bytes();

        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0x0800u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&dos_time.to_le_bytes());
        out.extend_from_slice(&dos_date.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&dos_time.to_le_bytes());
        central.extend_from_slice(&dos_date.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0u8; 12]); // extra, comment, disk, interne Attribute, externe Attribute (2+2+2+2+4)
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }

    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{ExportConfig, build_week};
    use crate::journal::{DayKind, Entry};
    use crate::settings::Settings;
    use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone, Utc};

    fn u16_at(b: &[u8], i: usize) -> usize {
        usize::from(u16::from_le_bytes([b[i], b[i + 1]]))
    }

    fn u32_at(b: &[u8], i: usize) -> usize {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize
    }

    /// Liest ein "stored"-ZIP über das zentrale Verzeichnis und gibt (Name, Inhalt) zurück
    pub fn read_zip(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        let eocd = zip.len() - 22;
        assert_eq!(u32_at(zip, eocd), 0x0605_4b50);
        let entries = u16_at(zip, eocd + 10);
        let mut pos = u32_at(zip, eocd + 16);
        let mut files = Vec::new();
        for _ in 0..entries {
            assert_eq!(u32_at(zip, pos), 0x0201_4b50);
            let crc = u32_at(zip, pos + 16) as u32;
            let size = u32_at(zip, pos + 24);
            let name_len = u16_at(zip, pos + 28);
            let local = u32_at(zip, pos + 42);
            let name = String::from_utf8(zip[pos + 46..pos + 46 + name_len].to_vec()).unwrap();

            assert_eq!(u32_at(zip, local), 0x0403_4b50);
            let local_name_len = u16_at(zip, local + 26);
            let local_extra = u16_at(zip, local + 28);
            let start = local + 30 + local_name_len + local_extra;
            let data = zip[start..start + size].to_vec();
            assert_eq!(crc32(&data), crc, "{name}");
            files.push((name, data));
            pos += 46 + name_len;
        }
        files
    }

    #[test]
    fn crc32_known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn zip_roundtrip() {
        let zip = zip_stored(&[("a.txt", b"hallo"), ("dir/b.xml", "Überraschung".as_bytes()), ("leer", b"")]);
        let files = read_zip(&zip);
        assert_eq!(files.len(), 3);
        assert_eq!(files[0], ("a.txt".to_string(), b"hallo".to_vec()));
        assert_eq!(files[1].0, "dir/b.xml");
        assert_eq!(String::from_utf8_lossy(&files[1].1), "Überraschung");
        assert!(files[2].1.is_empty());
        assert_eq!(&zip[..4], b"PK\x03\x04");
    }

    #[test]
    fn xml_text_is_safe() {
        assert_eq!(xml_text("a & b <c> \"d\""), "a &amp; b &lt;c&gt; &quot;d&quot;");
        assert_eq!(xml_text("Tab\there"), "Tab\there");
        assert_eq!(xml_text("x\u{0}y\u{8}z\u{b}\u{c}\u{1f}\u{fffe}"), "xyz");
        assert_eq!(xml_text("Umlaute äöü | Pipe"), "Umlaute äöü | Pipe");
    }

    #[test]
    fn paragraph_shapes() {
        assert_eq!(paragraph("", None, false), "<w:p></w:p>");
        assert_eq!(
            paragraph("a<b", Some("Heading2"), true),
            r#"<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">a&lt;b</w:t></w:r></w:p>"#
        );
        assert!(text_block("  \n ").contains("–"));
        assert_eq!(text_block("eins\r\nzwei").matches("<w:p>").count(), 2);
    }

    fn at(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        // Juli 2026: 20 = Montag
        Local.with_ymd_and_hms(2026, 7, d, h, m, 0).unwrap().with_timezone(&Utc)
    }

    /// Arbeitszeit 08:00-12:00 und 13:00-17:24, damit die gemessenen Zeiten glatte Werte ergeben
    fn sample_week(hours: f64, extra: Vec<Entry>) -> WeekData {
        let mut schedule = Settings::default().schedule().unwrap();
        schedule.blocks[1].1 = NaiveTime::from_hms_opt(17, 24, 0).unwrap();
        let config = ExportConfig { location: "@Noseryoung".into(), hours_per_day: hours, dir: None };
        let mut entries = vec![
            Entry::note(at(20, 11, 0), "Mich im Recur Projekt auf den neusten Stand gebracht", "checkin"),
            Entry::note(at(20, 14, 0), "Ich habe mich um einen Bug gekümmert welcher mit dem google-sign-in zu tun hatte", "checkin"),
            Entry::note(at(20, 15, 0), "Leon den Bug erklärt", "checkin"),
            Entry::note(at(20, 17, 24), "An der Account-page gearbeitet", "checkin"),
            Entry::note(at(21, 10, 24), "Account-Page fertiggestellt\nÜberlegt wie ich einen Kalender machen kann", "checkin"),
            Entry::note(at(21, 11, 54), "DB und Backend angepasst", "checkin"),
            Entry::note(at(21, 16, 24), "Add/EditTask angepasst", "checkin"),
            Entry::note(at(21, 17, 24), "Am Arbeitsjournal gearbeitet", "checkin"),
            Entry::day(at(22, 7, 0), DayKind::Schule),
            Entry::day(at(23, 7, 0), DayKind::Ferien),
            Entry::day(at(24, 7, 0), DayKind::Krank),
            Entry::reflection(
                at(24, 16, 0),
                "Diese Woche habe ich am Recur-Projekt gearbeitet.\nVor allem im Frontend.",
                "Ich habe gelernt: a < b & c | d",
                "Sehr lernfreudig",
            ),
        ];
        entries.extend(extra);
        build_week(NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(), &entries, &schedule, &config)
    }

    /// Texte aller Absätze in Dokumentreihenfolge (entschärfte Zeichen wieder zurückgewandelt)
    fn paragraphs(xml: &str) -> Vec<String> {
        let mut out = Vec::new();
        for chunk in xml.split("<w:p>").skip(1) {
            let chunk = chunk.split("</w:p>").next().unwrap();
            let mut text = String::new();
            let mut rest = chunk;
            while let Some(i) = rest.find("<w:t ") {
                let after = &rest[i..];
                let start = after.find('>').unwrap() + 1;
                let end = after.find("</w:t>").unwrap();
                text.push_str(&after[start..end]);
                rest = &after[end..];
            }
            out.push(text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&"));
        }
        out
    }

    /// Prüft grob, ob das XML wohlgeformt ist: Tags passen zusammen, kein loses `<` oder `&`
    fn assert_well_formed(xml: &str) {
        let mut stack: Vec<String> = Vec::new();
        let mut rest = xml;
        while let Some(i) = rest.find('<') {
            let text = &rest[..i];
            assert!(!text.contains('>'), "loses > im Text: {text:?}");
            let mut t = text;
            while let Some(a) = t.find('&') {
                let entity_end = t[a..].find(';').expect("& ohne ;");
                assert!(["&amp;", "&lt;", "&gt;", "&quot;"].contains(&&t[a..a + entity_end + 1]), "{}", &t[a..]);
                t = &t[a + entity_end + 1..];
            }
            let tag_end = rest[i..].find('>').expect("Tag nicht geschlossen") + i;
            let tag = &rest[i + 1..tag_end];
            assert!(!tag.contains('<'), "loses < im Tag");
            if tag.starts_with('?') || tag.ends_with('/') {
                // Deklaration oder selbstschliessendes Element
            } else if let Some(name) = tag.strip_prefix('/') {
                assert_eq!(stack.pop().as_deref(), Some(name), "schliessendes Tag passt nicht");
            } else {
                stack.push(tag.split_whitespace().next().unwrap().to_string());
            }
            rest = &rest[tag_end + 1..];
        }
        assert!(stack.is_empty(), "offene Tags: {stack:?}");
    }

    #[test]
    fn sample_week_snapshot() {
        let xml = document_xml(&sample_week(0.0, vec![]));
        assert_well_formed(&xml);
        let expected = [
            "Arbeitsjournal KW 30 2026",
            "20.07.26 – 26.07.26",
            "Tätigkeitstabelle",
            "Tag (wo und wann)",
            "Tätigkeit",
            "Zeit",
            "Zeit (Total)",
            // Montag
            "Montag",
            "20.07.26",
            "@Noseryoung",
            "- Mich im Recur Projekt auf den neusten Stand gebracht",
            "- Ich habe mich um einen Bug gekümmert welcher mit dem google-sign-in zu tun hatte",
            "- Leon den Bug erklärt",
            "- An der Account-page gearbeitet",
            "3.0h",
            "2.0h",
            "1.0h",
            "2.4h",
            "8.4h",
            // Dienstag
            "Dienstag",
            "21.07.26",
            "@Noseryoung",
            "- Account-Page fertiggestellt",
            "- Überlegt wie ich einen Kalender machen kann",
            "- DB und Backend angepasst",
            "- Add/EditTask angepasst",
            "- Am Arbeitsjournal gearbeitet",
            "2.4h",
            "",
            "1.5h",
            "3.5h",
            "1.0h",
            "8.4h",
            // Mittwoch bis Freitag ohne Einträge
            "Mittwoch",
            "22.07.26",
            "Schule",
            "–",
            "",
            "–",
            "Donnerstag",
            "23.07.26",
            "Ferien",
            "–",
            "",
            "–",
            "Freitag",
            "24.07.26",
            "Krank",
            "–",
            "",
            "–",
            // nach der Tabelle
            "",
            "Wochenrückblick",
            "Diese Woche habe ich am Recur-Projekt gearbeitet.",
            "Vor allem im Frontend.",
            "Reflexion",
            "Ich habe gelernt: a < b & c | d",
            "Stimmung der Woche",
            "Sehr lernfreudig",
            "Tabellenverzeichnis",
            "Tabelle 1 – Tätigkeitstabelle",
            "",
            "Datum: ______________________        Unterschrift: ______________________",
        ];
        assert_eq!(paragraphs(&xml), expected);
    }

    #[test]
    fn day_hours_snapshot() {
        // 8.4h pro Tag entsprechen genau der gemessenen Zeit der Beispielwoche
        let p = paragraphs(&document_xml(&sample_week(8.4, vec![])));
        let mon = p.iter().position(|t| t == "Montag").unwrap();
        assert_eq!(&p[mon + 7..mon + 12], ["3.0h", "2.0h", "1.0h", "2.4h", "8.4h"]);

        // mit 4.2h pro Tag werden dieselben Gewichte halbiert, die Summe stimmt genau
        let p = paragraphs(&document_xml(&sample_week(4.2, vec![])));
        let mon = p.iter().position(|t| t == "Montag").unwrap();
        assert_eq!(&p[mon + 7..mon + 12], ["1.5h", "1.0h", "0.5h", "1.2h", "4.2h"]);
        let tue = p.iter().position(|t| t == "Dienstag").unwrap();
        assert_eq!(&p[tue + 8..tue + 14], ["1.2h", "", "0.8h", "1.7h", "0.5h", "4.2h"]);
    }

    #[test]
    fn hostile_text_keeps_the_document_valid() {
        let evil = "Zelle | mit \"Pipe\" <w:p>&amp; ]]> \u{0}\u{8}\u{1b}[31m\nzweite Zeile\r\n\u{fffe}Ende";
        let extra = vec![
            Entry::note(at(22, 10, 0), evil, "quick"),
            Entry::commit(at(23, 9, 0), "re|po<>", &"a".repeat(40), "Commit <b>fett</b> & | mehr"),
            Entry::reflection(at(24, 17, 0), evil, evil, evil),
        ];
        let xml = document_xml(&sample_week(8.4, extra));
        assert_well_formed(&xml);
        assert!(!xml.contains('\u{0}') && !xml.contains('\u{8}') && !xml.contains('\u{1b}') && !xml.contains('\u{fffe}'));
        assert!(!xml.contains("<w:p>&amp;"), "Text darf kein Tag einschleusen");
        let p = paragraphs(&xml);
        assert!(p.contains(&"- Zelle | mit \"Pipe\" <w:p>&amp; ]]> [31m".to_string()), "{p:?}");
        assert!(p.contains(&"- zweite Zeile".to_string()));
        assert!(p.contains(&"- Commit re|po<>: Commit <b>fett</b> & | mehr".to_string()));
    }

    #[test]
    fn week_to_docx_is_a_complete_package() {
        let bytes = week_to_docx(&sample_week(8.4, vec![]));
        let files = read_zip(&bytes);
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["[Content_Types].xml", "_rels/.rels", "word/document.xml", "word/_rels/document.xml.rels", "word/styles.xml"]
        );
        for (name, data) in &files {
            assert_well_formed(std::str::from_utf8(data).unwrap_or_else(|_| panic!("{name} ist kein UTF-8")));
        }
        let document = String::from_utf8(files[2].1.clone()).unwrap();
        assert_eq!(document.matches("<w:tbl>").count(), 1);
        // Kopfzeile plus Mo bis Fr
        assert_eq!(document.matches("<w:tr>").count(), 6);
        assert!(document.contains(r#"<w:pgSz w:w="11906" w:h="16838"/>"#));
    }
}
