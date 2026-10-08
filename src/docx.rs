use crate::export::{WeekData, german_weekday};
use crate::log;
use flate2::read::DeflateDecoder;
use std::io::Read;

/// Platzhalter, die der Export kennt. Alles andere zwischen `{{` und `}}` wird aus der Vorlage entfernt.
/// Einzelwerte: NACHNAME, VORNAME, KW, JAHR, VON, BIS, UNTERSCHRIFTSDATUM.
/// Tabellenzeile pro Tag (die Zeile mit `{{TAG}}` wird geklont): TAG, DATUM, ORT, TAETIGKEIT, ZEIT, TOTAL.
/// Absätze mit mehreren Zeilen (der Absatz wird pro Zeile geklont): TAETIGKEIT, ZEIT, WOCHENRUECKBLICK,
/// REFLEXION, STIMMUNG.
const KNOWN: [&str; 16] = [
    "NACHNAME",
    "VORNAME",
    "KW",
    "JAHR",
    "VON",
    "BIS",
    "UNTERSCHRIFTSDATUM",
    "TAG",
    "DATUM",
    "ORT",
    "TAETIGKEIT",
    "ZEIT",
    "TOTAL",
    "WOCHENRUECKBLICK",
    "REFLEXION",
    "STIMMUNG",
];

const MAX_PLACEHOLDER: usize = 40;
/// Mehr Einträge darf ein .docx nicht haben, sonst ist es keine Vorlage
const MAX_ZIP_ENTRIES: usize = 2_000;
const MAX_PART_SIZE: u64 = 64 * 1024 * 1024;

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

fn u16_at(b: &[u8], i: usize) -> Option<usize> {
    Some(usize::from(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?)))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// Eine Datei aus dem ZIP
#[derive(Debug, Clone, PartialEq)]
pub struct ZipFile {
    pub name: String,
    pub data: Vec<u8>,
}

/// Liest ein ZIP über das zentrale Verzeichnis (Methoden "stored" und "deflate", kein ZIP64, keine Verschlüsselung)
/// und prüft Grösse und CRC jeder Datei. Die Reihenfolge bleibt wie im Archiv.
pub fn read_zip(zip: &[u8]) -> Result<Vec<ZipFile>, String> {
    let bad = || "Die Word-Vorlage ist keine gültige .docx-Datei (ZIP beschädigt)".to_string();

    // Ende-des-Verzeichnisses sitzt in den letzten 22 + 65535 Bytes
    let mut eocd = None;
    let last = zip.len().checked_sub(22).ok_or_else(bad)?;
    for pos in (last.saturating_sub(65_535)..=last).rev() {
        if u32_at(zip, pos) == Some(0x0605_4b50) {
            eocd = Some(pos);
            break;
        }
    }
    let eocd = eocd.ok_or_else(bad)?;
    let count = u16_at(zip, eocd + 10).ok_or_else(bad)?;
    let mut pos = u32_at(zip, eocd + 16).ok_or_else(bad)? as usize;
    if count > MAX_ZIP_ENTRIES {
        return Err(bad());
    }

    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(zip, pos) != Some(0x0201_4b50) {
            return Err(bad());
        }
        let flags = u16_at(zip, pos + 8).ok_or_else(bad)?;
        let method = u16_at(zip, pos + 10).ok_or_else(bad)?;
        let crc = u32_at(zip, pos + 16).ok_or_else(bad)?;
        let compressed = u32_at(zip, pos + 20).ok_or_else(bad)?;
        let size = u32_at(zip, pos + 24).ok_or_else(bad)?;
        let name_len = u16_at(zip, pos + 28).ok_or_else(bad)?;
        let extra_len = u16_at(zip, pos + 30).ok_or_else(bad)?;
        let comment_len = u16_at(zip, pos + 32).ok_or_else(bad)?;
        let local = u32_at(zip, pos + 42).ok_or_else(bad)? as usize;
        let name = String::from_utf8_lossy(zip.get(pos + 46..pos + 46 + name_len).ok_or_else(bad)?).into_owned();
        pos += 46 + name_len + extra_len + comment_len;

        if flags & 1 != 0 {
            return Err("Die Word-Vorlage ist verschlüsselt".to_string());
        }
        if compressed == u32::MAX || size == u32::MAX || u64::from(size) > MAX_PART_SIZE {
            return Err(format!("Die Word-Vorlage enthält eine zu grosse Datei ({name})"));
        }
        if u32_at(zip, local) != Some(0x0403_4b50) {
            return Err(bad());
        }
        let start = local + 30 + u16_at(zip, local + 26).ok_or_else(bad)? + u16_at(zip, local + 28).ok_or_else(bad)?;
        let raw = zip.get(start..start + compressed as usize).ok_or_else(bad)?;

        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut out = Vec::with_capacity(size as usize);
                // `take` begrenzt die Ausgabe, damit eine präparierte Datei den Speicher nicht füllt
                DeflateDecoder::new(raw)
                    .take(u64::from(size) + 1)
                    .read_to_end(&mut out)
                    .map_err(|e| format!("Die Word-Vorlage ist beschädigt ({name}): {e}"))?;
                out
            }
            other => return Err(format!("Die Word-Vorlage nutzt eine unbekannte Kompression ({other}) in {name}")),
        };
        if data.len() != size as usize || crc32(&data) != crc {
            return Err(format!("Die Word-Vorlage ist beschädigt (Prüfsumme von {name} stimmt nicht)"));
        }
        files.push(ZipFile { name, data });
    }
    Ok(files)
}

/// Beginn des Elements `<w:p>` bzw. `<w:p ...>` vor `before` (nicht `<w:pPr>` oder `<w:pStyle>`)
fn paragraph_start(xml: &str, before: usize) -> Option<usize> {
    let mut end = before;
    while let Some(i) = xml[..end].rfind("<w:p") {
        if matches!(xml.as_bytes().get(i + 4), Some(b'>' | b' ')) {
            return Some(i);
        }
        end = i;
    }
    None
}

/// Beginn der Tabellenzeile `<w:tr>` bzw. `<w:tr ...>` vor `before`
fn row_start(xml: &str, before: usize) -> Option<usize> {
    let mut end = before;
    while let Some(i) = xml[..end].rfind("<w:tr") {
        if matches!(xml.as_bytes().get(i + 5), Some(b'>' | b' ')) {
            return Some(i);
        }
        end = i;
    }
    None
}

/// Ort eines Textknotens `<w:t>` im Absatz
struct TextNode {
    /// Ende des öffnenden Tags (Position nach dem `>`)
    open_end: usize,
    /// Anfang des öffnenden Tags
    open_start: usize,
    text_end: usize,
}

fn text_nodes(p: &str) -> Vec<TextNode> {
    let mut nodes = Vec::new();
    let mut pos = 0;
    while let Some(i) = p[pos..].find("<w:t") {
        let start = pos + i;
        pos = start + 4;
        if !matches!(p.as_bytes().get(start + 4), Some(b'>' | b' ')) {
            continue;
        }
        let Some(gt) = p[start..].find('>') else { break };
        let open_end = start + gt + 1;
        if p[..open_end].ends_with("/>") {
            continue;
        }
        let Some(close) = p[open_end..].find("</w:t>") else { break };
        nodes.push(TextNode { open_start: start, open_end, text_end: open_end + close });
        pos = open_end + close;
    }
    nodes
}

/// Gültige Platzhalter-Namen: Buchstaben, Ziffern, `_`
fn placeholder_name(inner: &str) -> bool {
    !inner.is_empty() && inner.chars().count() <= MAX_PLACEHOLDER && inner.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Zeichenbereiche `[start, end)` aller `{{NAME}}` im Text
fn placeholder_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == '{' && chars[i + 1] == '{' {
            let limit = (i + 2 + MAX_PLACEHOLDER + 2).min(chars.len());
            if let Some(close) = (i + 2..limit.saturating_sub(1)).find(|&k| chars[k] == '}' && chars[k + 1] == '}') {
                let inner: String = chars[i + 2..close].iter().collect();
                if placeholder_name(&inner) {
                    spans.push((i, close + 2));
                    i = close + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    spans
}

/// Führt einen Platzhalter, den Word auf mehrere Läufe (`<w:r>`/`<w:t>`) verteilt hat, in den ersten Lauf zusammen.
/// Die Zeichen davor und danach bleiben in ihren Läufen, die Formatierung des ersten Laufs gilt.
fn merge_in_paragraph(p: &str) -> String {
    let nodes = text_nodes(p);
    // Zeichen mit dem Index ihres Textknotens
    let mut chars: Vec<(char, usize)> = Vec::new();
    for (n, node) in nodes.iter().enumerate() {
        chars.extend(p[node.open_end..node.text_end].chars().map(|c| (c, n)));
    }
    let plain: Vec<char> = chars.iter().map(|&(c, _)| c).collect();
    let spans = placeholder_spans(&plain);
    let split: Vec<_> = spans.iter().filter(|&&(a, b)| chars[a].1 != chars[b - 1].1).collect();
    if split.is_empty() {
        return p.to_string();
    }

    // Neue Texte pro Knoten: Zeichen eines zerteilten Platzhalters gehören dem Knoten seines ersten Zeichens
    let mut owner: Vec<usize> = chars.iter().map(|&(_, n)| n).collect();
    for &&(a, b) in &split {
        for slot in &mut owner[a..b] {
            *slot = chars[a].1;
        }
    }
    let mut texts = vec![String::new(); nodes.len()];
    for (k, &(c, _)) in chars.iter().enumerate() {
        texts[owner[k]].push(c);
    }

    let mut out = String::with_capacity(p.len());
    let mut last = 0;
    for (node, text) in nodes.iter().zip(&texts) {
        let old = &p[node.open_end..node.text_end];
        if old == text {
            continue;
        }
        out.push_str(&p[last..node.open_start]);
        let tag = &p[node.open_start..node.open_end];
        if text.starts_with(char::is_whitespace) || text.ends_with(char::is_whitespace) {
            if tag.contains("xml:space") {
                out.push_str(tag);
            } else {
                out.push_str(&tag[..tag.len() - 1]);
                out.push_str(" xml:space=\"preserve\">");
            }
        } else {
            out.push_str(tag);
        }
        out.push_str(text);
        last = node.text_end;
    }
    out.push_str(&p[last..]);
    out
}

/// Führt in jedem Absatz zerteilte `{{Platzhalter}}` zusammen (siehe `merge_in_paragraph`)
pub fn merge_placeholders(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut pos = 0;
    while let Some(i) = xml[pos..].find("<w:p") {
        let start = pos + i;
        if !matches!(xml.as_bytes().get(start + 4), Some(b'>' | b' ')) {
            out.push_str(&xml[pos..start + 4]);
            pos = start + 4;
            continue;
        }
        let Some(end) = xml[start..].find("</w:p>") else { break };
        let end = start + end + "</w:p>".len();
        out.push_str(&xml[pos..start]);
        out.push_str(&merge_in_paragraph(&xml[start..end]));
        pos = end;
    }
    out.push_str(&xml[pos..]);
    out
}

fn placeholder(name: &str) -> String {
    format!("{{{{{name}}}}}")
}

/// Entfernt `{{Name}}`, die der Export nicht kennt, damit nichts davon im Dokument stehen bleibt
fn strip_unknown(xml: &str) -> String {
    let chars: Vec<char> = xml.chars().collect();
    let mut out = String::with_capacity(xml.len());
    let mut last = 0;
    for (a, b) in placeholder_spans(&chars) {
        let name: String = chars[a + 2..b - 2].iter().collect();
        if KNOWN.contains(&name.as_str()) {
            continue;
        }
        log::warn(&format!("Vorlage: unbekannter Platzhalter {{{{{name}}}}} wird entfernt"));
        out.extend(&chars[last..a]);
        last = b;
    }
    out.extend(&chars[last..]);
    out
}

/// Ersetzt einen Einzelwert überall. Zeilenumbrüche werden zu Leerzeichen.
fn replace_scalar(xml: &str, name: &str, value: &str) -> String {
    let value = xml_text(&value.split_whitespace().collect::<Vec<_>>().join(" "));
    // Einmal von vorn nach hinten, damit ein Wert, der selbst wie ein Platzhalter aussieht, nicht ersetzt wird
    let ph = placeholder(name);
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find(&ph) {
        out.push_str(&rest[..i]);
        out.push_str(&value);
        rest = &rest[i + ph.len()..];
    }
    out.push_str(rest);
    out
}

/// Klont jeden Absatz mit `{{NAME}}` einmal pro Zeile (ohne Zeilen: einmal mit leerem Text)
fn expand_paragraph(xml: &str, name: &str, lines: &[String]) -> String {
    let ph = placeholder(name);
    let mut out = String::with_capacity(xml.len());
    let mut pos = 0;
    while let Some(i) = xml[pos..].find(&ph) {
        let at = pos + i;
        let (Some(start), Some(close)) = (paragraph_start(xml, at), xml[at..].find("</w:p>")) else {
            // kein Absatz drumherum: nur den Platzhalter selbst ersetzen
            out.push_str(&xml[pos..at]);
            out.push_str(&xml_text(lines.first().map_or("", String::as_str)));
            pos = at + ph.len();
            continue;
        };
        let end = at + close + "</w:p>".len();
        if start < pos {
            // der Absatz wurde schon als Teil eines vorigen behandelt
            out.push_str(&xml[pos..at + ph.len()]);
            pos = at + ph.len();
            continue;
        }
        let paragraph = &xml[start..end];
        out.push_str(&xml[pos..start]);
        let empty = [String::new()];
        for line in if lines.is_empty() { &empty[..] } else { lines } {
            out.push_str(&replace_scalar_raw(paragraph, &ph, &xml_text(line)));
        }
        pos = end;
    }
    out.push_str(&xml[pos..]);
    out
}

/// Wie `replace_scalar`, aber mit einem fertig geschützten Wert
fn replace_scalar_raw(xml: &str, ph: &str, escaped: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(i) = rest.find(ph) {
        out.push_str(&rest[..i]);
        out.push_str(escaped);
        rest = &rest[i + ph.len()..];
    }
    out.push_str(rest);
    out
}

/// Klont die Tabellenzeile mit `{{TAG}}` einmal pro Tag und füllt sie
fn expand_rows(xml: &str, week: &WeekData) -> Result<String, String> {
    let marker = placeholder("TAG");
    let at = xml.find(&marker).ok_or_else(|| {
        "Die Word-Vorlage enthält keine Tabellenzeile mit {{TAG}} (diese Zeile wird pro Tag wiederholt)".to_string()
    })?;
    let start = row_start(xml, at).ok_or_else(|| "{{TAG}} steht in der Vorlage nicht in einer Tabellenzeile".to_string())?;
    let end = at + xml[at..].find("</w:tr>").ok_or_else(|| "Tabellenzeile in der Vorlage nicht abgeschlossen".to_string())? + "</w:tr>".len();
    let template = &xml[start..end];

    let mut rows = String::new();
    for day in &week.days {
        // Erst die Einzelwerte, dann die Absätze: Texte der Einträge werden so nicht mehr als Platzhalter gelesen
        let mut row = replace_scalar(template, "TAG", german_weekday(day.date));
        row = replace_scalar(&row, "DATUM", &day.date.format("%d.%m.%y").to_string());
        row = replace_scalar(&row, "ORT", &day.place);
        row = replace_scalar(&row, "TOTAL", &day.total);
        row = expand_paragraph(&row, "TAETIGKEIT", &day.activities);
        row = expand_paragraph(&row, "ZEIT", &day.times);
        rows.push_str(&row);
    }
    Ok(format!("{}{rows}{}", &xml[..start], &xml[end..]))
}

/// Mehrzeiliger Text als Zeilen. Leerer Text wird zu einem Strich.
fn text_lines(text: &str) -> Vec<String> {
    let lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
    if lines.iter().all(|l| l.trim().is_empty()) { vec!["–".to_string()] } else { lines }
}

/// Füllt eine Teil-Datei (document.xml, Kopfzeile, ...) der Vorlage
fn fill_part(xml: &str, week: &WeekData, has_rows: bool) -> Result<String, String> {
    let fmt = |d: chrono::NaiveDate| d.format("%d.%m.%y").to_string();
    let mut xml = strip_unknown(&merge_placeholders(xml));
    for (name, value) in [
        ("NACHNAME", week.nachname.clone()),
        ("VORNAME", week.vorname.clone()),
        ("KW", week.week.to_string()),
        ("JAHR", week.year.to_string()),
        ("VON", fmt(week.from)),
        ("BIS", fmt(week.to)),
        ("UNTERSCHRIFTSDATUM", fmt(week.friday())),
    ] {
        xml = replace_scalar(&xml, name, &value);
    }
    if has_rows {
        xml = expand_rows(&xml, week)?;
    }
    for (name, text) in [
        ("WOCHENRUECKBLICK", &week.review),
        ("REFLEXION", &week.reflection),
        ("STIMMUNG", &week.mood),
    ] {
        xml = expand_paragraph(&xml, name, &text_lines(text));
    }
    Ok(xml)
}

/// Füllt die Word-Vorlage mit den Daten der Woche und gibt die fertige .docx-Datei zurück.
/// Bearbeitet werden `word/document.xml` sowie alle Kopf- und Fusszeilen, die `{{` enthalten.
pub fn fill_template(template: &[u8], week: &WeekData) -> Result<Vec<u8>, String> {
    let files = read_zip(template)?;
    if !files.iter().any(|f| f.name == "word/document.xml") {
        return Err("Die Word-Vorlage enthält kein word/document.xml".to_string());
    }

    let mut out: Vec<(String, Vec<u8>)> = Vec::with_capacity(files.len());
    for file in files {
        let is_part = file.name.starts_with("word/") && file.name.ends_with(".xml") && !file.name.contains("/_rels/");
        let data = match std::str::from_utf8(&file.data) {
            Ok(xml) if is_part && (xml.contains("{{") || file.name == "word/document.xml") => {
                fill_part(xml, week, file.name == "word/document.xml")?.into_bytes()
            }
            _ => file.data,
        };
        out.push((file.name, data));
    }
    let refs: Vec<(&str, &[u8])> = out.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    Ok(zip_stored(&refs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::tests::{at, config, date, override_day, week_file};
    use crate::export::{build_week, default_template};
    use crate::journal::{Entry, WeekFile, WeekTexts};

    fn week_data(file: &WeekFile) -> WeekData {
        build_week(date(20), file, &config())
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

    /// Texte aller Absätze in Dokumentreihenfolge (geschützte Zeichen wieder zurückgewandelt)
    fn paragraphs(xml: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut pos = 0;
        while let Some(i) = xml[pos..].find("<w:p") {
            let start = pos + i;
            pos = start + 4;
            if !matches!(xml.as_bytes().get(start + 4), Some(b'>' | b' ')) {
                continue;
            }
            let end = start + xml[start..].find("</w:p>").unwrap();
            let p = &xml[start..end];
            let text: String = text_nodes(p).iter().map(|n| &p[n.open_end..n.text_end]).collect();
            out.push(text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&"));
            pos = end;
        }
        out
    }

    fn part(files: &[ZipFile], name: &str) -> String {
        String::from_utf8(files.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("{name} fehlt")).data.clone()).unwrap()
    }

    #[test]
    fn crc32_known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn zip_roundtrip() {
        let zip = zip_stored(&[("a.txt", b"hallo"), ("dir/b.xml", "Überraschung".as_bytes()), ("leer", b"")]);
        let files = read_zip(&zip).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0], ZipFile { name: "a.txt".into(), data: b"hallo".to_vec() });
        assert_eq!(files[1].name, "dir/b.xml");
        assert_eq!(String::from_utf8_lossy(&files[1].data), "Überraschung");
        assert!(files[2].data.is_empty());
        assert_eq!(&zip[..4], b"PK\x03\x04");
    }

    #[test]
    fn zip_reader_handles_deflate() {
        // Die mitgelieferte Vorlage ist deflate-komprimiert
        let template = default_template();
        assert_eq!(u16_at(template, 8), Some(8), "erster Eintrag ist nicht deflate-komprimiert");
        let files = read_zip(template).unwrap();
        let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"word/document.xml") && names.contains(&"word/header1.xml"), "{names:?}");
        assert_eq!(names[0], "[Content_Types].xml");
        assert!(part(&files, "word/document.xml").contains("{{TAG}}"));
    }

    #[test]
    fn zip_reader_rejects_garbage() {
        assert!(read_zip(b"").is_err());
        assert!(read_zip(b"das ist kein zip, nur Text, aber lang genug fuer den Kopf").is_err());
        let mut zip = zip_stored(&[("a.txt", b"hallo")]);
        // Prüfsumme im zentralen Verzeichnis kaputt machen
        let central = zip.windows(4).position(|w| w == [0x50, 0x4b, 0x01, 0x02]).unwrap();
        zip[central + 16] ^= 0xFF;
        assert!(read_zip(&zip).unwrap_err().contains("Prüfsumme"));
        // abgeschnitten
        let good = zip_stored(&[("a.txt", b"hallo")]);
        assert!(read_zip(&good[..good.len() - 10]).is_err());
    }

    #[test]
    fn xml_text_is_safe() {
        assert_eq!(xml_text("a & b <c> \"d\""), "a &amp; b &lt;c&gt; &quot;d&quot;");
        assert_eq!(xml_text("Tab\there"), "Tab\there");
        assert_eq!(xml_text("x\u{0}y\u{8}z\u{b}\u{c}\u{1f}\u{fffe}"), "xyz");
        assert_eq!(xml_text("Umlaute äöü | Pipe"), "Umlaute äöü | Pipe");
    }

    #[test]
    fn merges_placeholders_split_by_word() {
        // Word zerlegt "{{TAETIGKEIT}}" gern in mehrere Läufe
        let xml = r#"<w:p><w:pPr/><w:r><w:rPr><w:b/></w:rPr><w:t>{{TAET</w:t></w:r><w:r><w:t>IGK</w:t></w:r><w:proofErr w:type="spellEnd"/><w:r><w:t>EIT}}</w:t></w:r></w:p>"#;
        let merged = merge_placeholders(xml);
        assert!(merged.contains("<w:t>{{TAETIGKEIT}}</w:t>"), "{merged}");
        // die Formatierung des ersten Laufs bleibt, die anderen werden leer
        assert!(merged.contains("<w:rPr><w:b/></w:rPr><w:t>{{TAETIGKEIT}}</w:t>"));
        assert_eq!(merged.matches("<w:t></w:t>").count(), 2);
        assert_well_formed(&merged);
        assert_eq!(paragraphs(&merged), ["{{TAETIGKEIT}}"]);
    }

    #[test]
    fn merge_keeps_surrounding_text_and_spaces() {
        let xml = r#"<w:p><w:r><w:t xml:space="preserve">KW {</w:t></w:r><w:r><w:t>{K</w:t></w:r><w:r><w:t xml:space="preserve">W}} / {{JA</w:t></w:r><w:r><w:t>HR}} Ende </w:t></w:r></w:p>"#;
        let merged = merge_placeholders(xml);
        assert_eq!(paragraphs(&merged), ["KW {{KW}} / {{JAHR}} Ende "]);
        assert!(merged.contains(r#"<w:t xml:space="preserve">KW {{KW}}</w:t>"#), "{merged}");
        assert!(merged.contains(r#"<w:t xml:space="preserve"> / {{JAHR}}</w:t>"#), "{merged}");
        // Ende mit Leerzeichen bekommt xml:space nachgereicht
        let xml = r#"<w:p><w:r><w:t>{{A</w:t></w:r><w:r><w:t>B}} </w:t></w:r></w:p>"#;
        let merged = merge_placeholders(xml);
        assert!(merged.contains(r#"<w:t xml:space="preserve"> </w:t>"#), "{merged}");
        assert_eq!(paragraphs(&merged), ["{{AB}} "]);
        assert_well_formed(&merged);
    }

    #[test]
    fn merge_leaves_whole_and_foreign_text_alone() {
        let xml = r#"<w:body><w:p><w:r><w:t>{{TAG}}</w:t></w:r></w:p><w:p><w:r><w:t>Klammern { { und }} { x</w:t></w:r><w:r><w:t>{{ kein Name }}</w:t></w:r></w:p><w:p/></w:body>"#;
        assert_eq!(merge_placeholders(xml), xml);
        // Absätze mit Attributen und Eigenschaften werden erkannt, <w:pPr> und <w:pStyle> nicht für Absätze gehalten
        let split = r#"<w:p w14:paraId="1"><w:pPr><w:pStyle w:val="x"/></w:pPr><w:r><w:t>{</w:t></w:r><w:r><w:t>{ZEIT}</w:t></w:r><w:r><w:t>}</w:t></w:r></w:p>"#;
        let merged = merge_placeholders(split);
        assert_eq!(paragraphs(&merged), ["{{ZEIT}}"]);
        assert!(merged.starts_with(r#"<w:p w14:paraId="1"><w:pPr><w:pStyle w:val="x"/></w:pPr>"#));
        assert_eq!(merge_placeholders(""), "");
        assert_eq!(merge_placeholders("<w:pPr><w:p>offen"), "<w:pPr><w:p>offen");
    }

    #[test]
    fn expands_paragraphs_per_line() {
        let xml = r#"<w:body><w:p w:x="1"><w:pPr><w:pStyle w:val="a"/></w:pPr><w:r><w:t>{{ZEIT}}</w:t></w:r></w:p><w:p><w:r><w:t>danach</w:t></w:r></w:p></w:body>"#;
        let lines = vec!["2.4h".to_string(), String::new(), "a < b & \"c\"".to_string()];
        let out = expand_paragraph(xml, "ZEIT", &lines);
        assert_eq!(paragraphs(&out), ["2.4h", "", "a < b & \"c\"", "danach"]);
        assert_eq!(out.matches(r#"<w:p w:x="1"><w:pPr>"#).count(), 3);
        assert!(out.contains("a &lt; b &amp; &quot;c&quot;"));
        assert_well_formed(&out);
        // ohne Zeilen: ein leerer Absatz
        assert_eq!(paragraphs(&expand_paragraph(xml, "ZEIT", &[])), ["", "danach"]);
        // ein Wert, der wie ein Platzhalter aussieht, wird nicht noch einmal ersetzt
        let out = expand_paragraph(xml, "ZEIT", &["{{ZEIT}}".to_string()]);
        assert_eq!(paragraphs(&out), ["{{ZEIT}}", "danach"]);
    }

    #[test]
    fn clones_the_table_row_per_day() {
        let xml = concat!(
            "<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Kopf</w:t></w:r></w:p></w:tc></w:tr>",
            "<w:tr w:rsidR=\"1\"><w:trPr><w:trHeight w:val=\"754\"/></w:trPr>",
            "<w:tc><w:p><w:r><w:t>{{TAG}}</w:t></w:r></w:p><w:p><w:r><w:t>{{DATUM}}</w:t></w:r></w:p><w:p><w:r><w:t>{{ORT}}</w:t></w:r></w:p></w:tc>",
            "<w:tc><w:p><w:r><w:t>{{TAETIGKEIT}}</w:t></w:r></w:p></w:tc>",
            "<w:tc><w:p><w:r><w:t>{{ZEIT}}</w:t></w:r></w:p></w:tc>",
            "<w:tc><w:p><w:r><w:t>{{TOTAL}}</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>danach</w:t></w:r></w:p>"
        );
        let file = week_file(vec![
            Entry::manuell(at(20, 10, 0), "Eins\nZwei", 3.0),
            Entry::manuell(at(20, 14, 0), "Drei", 5.4),
        ]);
        let week = week_data(&file);
        let out = expand_rows(xml, &week).unwrap();
        assert_well_formed(&out);
        // Kopf, fünf Tage, Absatz danach
        assert_eq!(out.matches("<w:tr>").count() + out.matches("<w:tr ").count(), 6);
        assert_eq!(out.matches("<w:trHeight").count(), 5);
        let p = paragraphs(&out);
        assert_eq!(
            &p[..11],
            ["Kopf", "Montag", "20.07.26", "@Noser Young", "- Eins", "- Zwei", "- Drei", "3.0h", "", "5.4h", "8.4h"]
        );
        // Dienstag ohne Einträge: Strich bei Tätigkeit und Total
        assert_eq!(&p[11..17], ["Dienstag", "21.07.26", "@Noser Young", "–", "", "–"]);
        assert_eq!(p.last().unwrap(), "danach");
        assert!(!out.contains("{{"));

        assert!(expand_rows("<w:p><w:r><w:t>nichts</w:t></w:r></w:p>", &week).unwrap_err().contains("{{TAG}}"));
        assert!(expand_rows("<w:p><w:r><w:t>{{TAG}}</w:t></w:r></w:p>", &week).is_err());
    }

    #[test]
    fn unknown_placeholders_are_removed_but_user_braces_stay() {
        let xml = "<w:t>a {{GIBTSNICHT}} b {{TAG}} c { {x}} d</w:t>";
        assert_eq!(strip_unknown(xml), "<w:t>a  b {{TAG}} c { {x}} d</w:t>");
    }

    fn sample_file() -> WeekFile {
        // Beispielwoche KW30, jeden Tag 8.4h bei Noser Young
        let rows: [(u32, &[(u32, &str, f64)]); 5] = [
            (20, &[(11, "Mich im Recur Projekt auf den neusten Stand gebracht", 3.0), (14, "Ich habe mich um einen Bug gekümmert welcher mit dem google-sign-in zu tun hatte", 2.0), (15, "Leon den Bug erklärt", 1.0), (17, "An der Account-page gearbeitet", 2.4)]),
            (21, &[(10, "Account-Page fertiggestellt\nÜberlegt wie ich einen Kalender machen kann", 2.4), (12, "DB und Backend angepasst", 1.5), (14, "Add/EditTask angepasst", 3.5), (17, "Am Arbeitsjournal gearbeitet", 1.0)]),
            (22, &[(11, "Änderungen an Edit und AddTask vorgenommen", 2.4), (16, "Mit dem Kalender begonnen", 6.0)]),
            (23, &[(10, "Kalender überarbeitet", 6.0), (14, "Backend angepasst", 1.4), (17, "Am Arbeitsjournal gearbeitet", 1.0)]),
            (24, &[(10, "Task-Zelle im Kalender eine Detail-Ansicht gegeben", 3.0), (14, "Login Form angepasst", 1.4), (15, "Letzte Änderungen vorgenommen", 2.5), (17, "Arbeitsjournal vervollständigt", 1.5)]),
        ];
        let mut entries = Vec::new();
        for (day, list) in rows {
            for &(hour, text, hours) in list {
                entries.push(Entry::manuell(at(day, hour, 0), text, hours));
            }
        }
        let mut file = week_file(entries);
        file.texte = WeekTexts {
            rueckblick: "Diese Woche habe ich am Recur-Projekt gearbeitet.\nVor allem im Frontend & Backend.".into(),
            reflexion: "Ich habe gelernt: a < b & c | d".into(),
            stimmung: "Sehr lernfreudig".into(),
        };
        file
    }

    /// Entpackt die fertige .docx, prüft alle XML-Teile und gibt Dokument und Kopfzeile zurück
    fn unpack_and_check(docx: &[u8]) -> (String, String) {
        let files = read_zip(docx).unwrap();
        for f in &files {
            if f.name.ends_with(".xml") || f.name.ends_with(".rels") {
                let xml = std::str::from_utf8(&f.data).unwrap_or_else(|_| panic!("{} ist kein UTF-8", f.name));
                assert_well_formed(xml);
                assert!(!xml.contains("{{"), "{{ übrig in {}", f.name);
            }
        }
        // alle Teile der Vorlage sind noch da
        let original: Vec<_> = read_zip(default_template()).unwrap().into_iter().map(|f| f.name).collect();
        let names: Vec<_> = files.iter().map(|f| f.name.clone()).collect();
        assert_eq!(names, original);
        (part(&files, "word/document.xml"), part(&files, "word/header1.xml"))
    }

    #[test]
    fn exports_the_sample_week() {
        let week = week_data(&sample_file());
        let docx = fill_template(default_template(), &week).unwrap();
        let (document, header) = unpack_and_check(&docx);

        let head = paragraphs(&header);
        assert!(head[0].contains("Jemuel Maurer") && head[0].contains("KW 30 / 2026"), "{head:?}");

        let p = paragraphs(&document);
        let from = p.iter().position(|t| t == "Montag").unwrap();
        assert_eq!(
            &p[from..from + 12],
            [
                "Montag",
                "20.07.26",
                "@Noser Young",
                "- Mich im Recur Projekt auf den neusten Stand gebracht",
                "- Ich habe mich um einen Bug gekümmert welcher mit dem google-sign-in zu tun hatte",
                "- Leon den Bug erklärt",
                "- An der Account-page gearbeitet",
                "3.0h",
                "2.0h",
                "1.0h",
                "2.4h",
                "8.4h",
            ]
        );
        // Dienstag: eine Notiz mit zwei Zeilen, die Zeit steht nur auf der ersten
        let tue = p.iter().position(|t| t == "Dienstag").unwrap();
        assert_eq!(&p[tue + 1..tue + 5], ["21.07.26", "@Noser Young", "- Account-Page fertiggestellt", "- Überlegt wie ich einen Kalender machen kann"]);
        assert_eq!(&p[tue + 8..tue + 14], ["2.4h", "", "1.5h", "3.5h", "1.0h", "8.4h"]);
        for day in ["Mittwoch", "Donnerstag", "Freitag"] {
            let i = p.iter().position(|t| t == day).unwrap();
            assert_eq!(p[i + 2], "@Noser Young", "{day}");
        }
        assert_eq!(p.iter().filter(|t| *t == "8.4h").count(), 5);

        // Wochentexte, mehrzeilig geklont und geschützt
        let r = p.iter().position(|t| t == "Diese Woche habe ich am Recur-Projekt gearbeitet.").unwrap();
        assert_eq!(p[r + 1], "Vor allem im Frontend & Backend.");
        assert!(p.contains(&"Ich habe gelernt: a < b & c | d".to_string()));
        assert!(p.contains(&"Sehr lernfreudig".to_string()));
        // Unterschrift: Freitag der Woche
        assert!(p.contains(&"24.07.26".to_string()));
        // Kopf der Tabelle und Verzeichnis aus der Vorlage bleiben
        assert!(p.iter().any(|t| t.trim() == "Tätigkeitstabelle") && p.iter().any(|t| t.contains("Unterschrift")));
        assert_eq!(document.matches("<w:tbl>").count(), 1);
        // Kopfzeile + 5 Tage
        assert_eq!(document.matches("<w:tr ").count() + document.matches("<w:tr>").count(), 6);
        assert!(document.contains(r#"<w:pgSz w:w="11906" w:h="16838"/>"#));
    }

    #[test]
    fn exports_a_pure_holiday_week() {
        let mut file = week_file(vec![]);
        for d in 20..=24 {
            override_day(&mut file, d, "Ferien");
        }
        let week = week_data(&file);
        let (document, _) = unpack_and_check(&fill_template(default_template(), &week).unwrap());
        let p = paragraphs(&document);
        assert_eq!(p.iter().filter(|t| *t == "- Ich habe die Ferien genossen").count(), 5);
        assert_eq!(p.iter().filter(|t| *t == "@Ferien").count(), 5);
        assert!(p.contains(&"Es gibt keinen Wochenrückblick, da ich in den Ferien war.".to_string()));
        assert!(p.contains(&"Es gibt keine Reflexion, da ich in den Ferien war.".to_string()));
        assert!(p.contains(&"Es gibt keine Stimmung der Woche, da ich in den Ferien war.".to_string()));
        // keine Stunden, kein Tagestotal
        assert!(!p.iter().any(|t| t.ends_with('h') && t.chars().next().is_some_and(|c| c.is_ascii_digit())));
        assert!(!p.contains(&"–".to_string()));
    }

    #[test]
    fn exports_a_pure_uek_week() {
        let mut file = week_file(vec![]);
        for d in 20..=24 {
            override_day(&mut file, d, "üK");
        }
        let week = week_data(&file);
        let (document, _) = unpack_and_check(&fill_template(default_template(), &week).unwrap());
        let p = paragraphs(&document);
        assert_eq!(p.iter().filter(|t| *t == "- Wir hatten üK").count(), 5);
        assert_eq!(p.iter().filter(|t| *t == "@üK").count(), 5);
        assert!(p.contains(&"Diese Woche gibt es keinen Wochenrückblick, da wir einen üK hatten.".to_string()));
        assert!(p.contains(&"Diese Woche gibt es keine Wochenreflexion, da wir einen üK hatten.".to_string()));
        assert!(p.contains(&"Diese Woche gibt es keine Stimmung der Woche, da wir einen üK hatten.".to_string()));
        assert!(!p.iter().any(|t| t.ends_with('h') && t.chars().next().is_some_and(|c| c.is_ascii_digit())));
    }

    #[test]
    fn empty_week_uses_dashes() {
        let week = week_data(&week_file(vec![]));
        let (document, _) = unpack_and_check(&fill_template(default_template(), &week).unwrap());
        let p = paragraphs(&document);
        // Tätigkeit und Total pro Tag, dazu die drei Wochentexte
        assert_eq!(p.iter().filter(|t| *t == "–").count(), 5 + 5 + 3);
    }

    #[test]
    fn hostile_text_keeps_the_document_valid() {
        let evil = "Zelle | mit \"Pipe\" <w:p>&amp; ]]> \u{0}\u{8}\u{1b}[31m\nzweite Zeile\r\n\u{fffe}Ende {{TAG}} {{KW}}";
        let mut file = week_file(vec![
            Entry::manuell(at(22, 10, 0), evil, 1.0),
            Entry::commit(at(23, 9, 0), "re|po<>", &"a".repeat(40), "Commit <b>fett</b> & | mehr"),
        ]);
        file.eintraege[1].stunden = 0.5;
        file.texte = WeekTexts { rueckblick: evil.into(), reflexion: evil.into(), stimmung: evil.into() };
        let mut c = config();
        c.nachname = "O'Brien <&>".into();
        let week = build_week(date(20), &file, &c);
        let docx = fill_template(default_template(), &week).unwrap();

        let files = read_zip(&docx).unwrap();
        let document = part(&files, "word/document.xml");
        assert_well_formed(&document);
        assert_well_formed(&part(&files, "word/header1.xml"));
        assert!(!document.contains('\u{0}') && !document.contains('\u{8}') && !document.contains('\u{1b}') && !document.contains('\u{fffe}'));
        assert!(!document.contains("<w:p>&amp;"), "Text darf kein Tag einschleusen");
        let p = paragraphs(&document);
        assert!(p.contains(&"- Zelle | mit \"Pipe\" <w:p>&amp; ]]> [31m".to_string()), "{p:?}");
        assert!(p.contains(&"- zweite Zeile".to_string()));
        assert!(p.contains(&"- Ende {{TAG}} {{KW}}".to_string()), "Platzhalter im Text bleiben Text");
        assert!(p.contains(&"- Commit re|po<>: Commit <b>fett</b> & | mehr".to_string()));
        assert!(paragraphs(&part(&files, "word/header1.xml"))[0].contains("Jemuel O'Brien <&>"));
    }

    /// Spaltet `{{NAME}}` in der Vorlage wie Word in drei Läufe auf
    fn split_in_template(template: &[u8], names: &[&str]) -> Vec<u8> {
        let files = read_zip(template).unwrap();
        let out: Vec<(String, Vec<u8>)> = files
            .into_iter()
            .map(|f| {
                let mut data = f.data;
                if f.name.starts_with("word/") && f.name.ends_with(".xml") {
                    let mut xml = String::from_utf8(data.clone()).unwrap();
                    for name in names {
                        let (a, b) = name.split_at(1);
                        let (b, c) = b.split_at(b.len() / 2);
                        xml = xml.replace(
                            &format!("{{{{{name}}}}}"),
                            &format!("{{{{{a}</w:t></w:r><w:proofErr w:type=\"spellStart\"/><w:r><w:rPr><w:i/></w:rPr><w:t>{b}</w:t></w:r><w:r><w:t>{c}}}}}"),
                        );
                    }
                    data = xml.into_bytes();
                }
                (f.name, data)
            })
            .collect();
        let refs: Vec<(&str, &[u8])> = out.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
        zip_stored(&refs)
    }

    #[test]
    fn split_placeholders_in_a_real_template_still_work() {
        let all = ["TAG", "DATUM", "ORT", "TAETIGKEIT", "ZEIT", "TOTAL", "WOCHENRUECKBLICK", "REFLEXION", "STIMMUNG", "UNTERSCHRIFTSDATUM", "VORNAME", "NACHNAME", "KW", "JAHR"];
        let split = split_in_template(default_template(), &all);
        // die gespaltene Vorlage enthält wirklich keinen ganzen Platzhalter mehr
        let files = read_zip(&split).unwrap();
        assert!(!part(&files, "word/document.xml").contains("{{TAG}}"));
        assert!(part(&files, "word/document.xml").contains("{{T</w:t>"));

        let week = week_data(&sample_file());
        let from_whole = fill_template(default_template(), &week).unwrap();
        let from_split = fill_template(&split, &week).unwrap();
        let (doc_a, head_a) = unpack_and_check(&from_whole);
        let (doc_b, head_b) = unpack_and_check(&from_split);
        assert_eq!(paragraphs(&doc_a), paragraphs(&doc_b));
        assert_eq!(paragraphs(&head_a), paragraphs(&head_b));
    }

    #[test]
    fn template_without_row_marker_is_rejected() {
        let zip = zip_stored(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("word/document.xml", br#"<w:document><w:body><w:p><w:r><w:t>Hallo</w:t></w:r></w:p></w:body></w:document>"#),
        ]);
        let week = week_data(&week_file(vec![]));
        assert!(fill_template(&zip, &week).unwrap_err().contains("{{TAG}}"));
        assert!(fill_template(&zip_stored(&[("a.txt", b"x")]), &week).unwrap_err().contains("document.xml"));
        assert!(fill_template(b"kein zip", &week).is_err());
    }

    #[test]
    fn custom_template_with_headers_and_footers() {
        let zip = zip_stored(&[
            ("word/document.xml", br#"<w:document><w:body><w:tbl><w:tr><w:tc><w:p><w:r><w:t>{{TAG}} {{DATUM}} {{ORT}}</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>{{TAETIGKEIT}}</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>{{ZEIT}}</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>{{TOTAL}}</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t>{{WOCHENRUECKBLICK}}</w:t></w:r></w:p></w:body></w:document>"#),
            ("word/footer2.xml", br#"<w:ftr><w:p><w:r><w:t>{{VON}} - {{BIS}} {{UNBEKANNT}}</w:t></w:r></w:p></w:ftr>"#),
            ("word/media.bin", b"{{TAG}} bleibt"),
        ]);
        let week = week_data(&sample_file());
        let out = read_zip(&fill_template(&zip, &week).unwrap()).unwrap();
        assert_eq!(paragraphs(&part(&out, "word/footer2.xml")), ["20.07.26 - 26.07.26 "]);
        assert_eq!(part(&out, "word/media.bin"), "{{TAG}} bleibt");
        let p = paragraphs(&part(&out, "word/document.xml"));
        assert_eq!(p[0], "Montag 20.07.26 @Noser Young");
        assert!(p.contains(&"Vor allem im Frontend & Backend.".to_string()));
    }
}
