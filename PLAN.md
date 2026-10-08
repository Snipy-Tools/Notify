# Plan: Arbeitsjournal als Always-on-top-Leiste (Basis: Notify)

Basis ist `C:\Work\2.Lehrjahr\Notify` (Rust, tao + wry + tray-icon). Nur Windows, offline, keine KI.
Grundlage: Audit von Notify/Recapr und die Entscheidungen Q1-Q23 aus dem Interview.

## Festgelegte Entscheidungen
- Mini-Fenster: rahmenlos, immer im Vordergrund, verschiebbar, merkt sich Position, einklappbar. Hotkey `Ctrl+Alt+J` blendet ein/aus und fokussiert die Eingabezeile.
- Eintrag: Text + Dauer (z. B. `Bug gefixt 2.4`), Enter speichert, landet beim heutigen Tag. Frühere Tage/Wochen per Pfeile editierbar. Tagessumme gegen Soll (Standard 8.4 h, pro Ort einstellbar), gelbe Warnung bei Abweichung. Knopf "Rest auf letzte Zeile"; Auto-Auffüllen als Einstellung, Standard aus.
- Ort pro Tag: `@Noser Young`, `@Gibb`, `@üK`, `@Ferien`. Standardplan Mo-Mi `@Noser Young`, Do-Fr `@Gibb`. Override pro Woche im Hauptfenster (Dropdown pro Tag).
- Reine Ferien-/üK-Woche: Tätigkeit und Wochenrückblick/Reflexion/Stimmung werden automatisch mit festen Sätzen gefüllt (üK: "Wir hatten üK", "…keine Wochenreflexion, da wir einen üK hatten"). Mischwoche: selbst schreiben. Ferien + üK: üK gewinnt. Stunden bei Ferien/üK leer. Texte editierbar.
- Einstellungs-Tab "Vorlage": Texte, Ortsnamen, Name/Vorname, Tagessoll, eigene .docx-Vorlage wählen.
- Export: bestehendes Arbeitsjournal-.docx als Vorlage mit Platzhaltern. Dateiname `Arbeitsjournal-<nachname>-<vorname>-<jahr>-<KW>`, Unterschriftsdatum = Freitag.
- Speicher: eine JSON pro Kalenderwoche (`2026-KW30.json`) in frei wählbarem Ordner. Einmaliger Import der alten JSONL.
- Erinnerungen: stündlich in der Arbeitszeit (bereits in timer.rs), nicht bei Ferien/üK/gesperrtem Bildschirm, zusätzlich nach neuem Git-Commit. Anklickbarer Hinweis fokussiert die Eingabe, mit Snooze.
- Feld `quelle` pro Eintrag (`manuell`, `commit`, später `kalender`). Outlook-Import später.
- Nicht-Ziele: Login, KI, Mobile, Team-Features, Kalender-Integration in v1.

## Design
Gewählt: Recur-Look (shadcn `base-nova`, neutral, Dark, Geist, lucide-Icons, Radius 10px). Entwürfe: https://claude.ai/artifact/96Tq1w4NJNA5N3NBtD9ydU (untere Reihe "Recur-…").
- Umsetzung als Plain-CSS-Nachbau mit denselben Tokens (Hex aus `Ferien_Projekt_Recur/recur/frontend/src/globals.css`), nicht als React/shadcn: `Notify` nutzt wry + `include_str!`-HTML, ein React-Build wäre ein eigener Aufbau.
- Keine farbigen Hinweisboxen. Abweichung vom Tagessoll wird neutral gezeigt ("6.4 / 8.4 h, noch 2.0 h offen"), Farbe höchstens als kleiner Text-/Punkt-Akzent.

## Annahmen (nicht explizit bestätigt, änderbar)
- ENTSCHIEDEN (vom Nutzer bestätigt): Das Claude-Widget (`widget.rs`, `server.rs`, `ui/widget.html`) wird weggelassen/entfernt. Der Server dient aktuell auch als Sperre gegen eine zweite Instanz, dafür braucht es eine andere Sperre (z. B. benanntes Mutex). Das gehört in Phase 3, Schritt 6.
- Plain-CSS statt React (siehe Design).

## Risiko
- Aufgeteilte `{{Platzhalter}}` in Word-XML (grösstes technisches Risiko, siehe Phase 2).

## Phasen und Modelle

### Phase 0 - Planung (Opus)
1. Datenmodell v2: Wochen-Datei (Tage mit Ort und Abweichung, Einträge mit Text/Stunden/quelle/Zeit, Reflexion), Settings v2, Migrationspfad.
2. Platzhalter-Vertrag für die Vorlage (zu klonende Tabellenzeile, Feldnamen, Strategie gegen aufgeteilte Platzhalter).
3. UI-Konzept der Leiste (eingeklappt/ausgeklappt, Fokusregeln, Ort, Tagessumme, Warnung) - siehe Design.

### Phase 1 - Fachmodell (Sonnet)
1. `journal.rs`: Speicher pro Kalenderwoche + Importer. Woche aus lokaler Zeit ableiten (Zeitstempel sind UTC).
2. Wochenplan/Ort-Logik: `effective_kind` (timer.rs:28) durch Ort-Auflösung ersetzen.
3. Dauer pro Eintrag, Tagessumme, "Rest auffüllen" (`distribute` aus export.rs wiederverwenden). Etwa 15 Tests in export.rs/docx.rs anpassen.

### Phase 2 - Export (Sonnet)
4. `docx.rs`: Vorlage entpacken (Deflate, `flate2` oder `miniz_oxide`), aufgeteilte Platzhalter zusammenführen, ersetzen, Tabellenzeilen klonen, packen.
5. Auto-Texte für Ferien-/üK-Wochen, Dateiname, Datum Freitag.

### Phase 3 - Oberfläche (Sonnet)
6. Leiste aus `widget.rs` ableiten (Position, Einrasten, Einklappen), Fokus nur per Hotkey/Klick, Hotkey einstellbar.
7. Eingabezeile, Tagesansicht, Wochenansicht, Wochentexte, Override-Dropdown, Tab "Vorlage".
8. Erinnerung nach neuem Commit (recap.rs:215), anklickbarer Hinweis mit Snooze.

### Kleine Tasks (Haiku)
- Dateiname und Freitagsdatum, feste Texte, Feld `quelle`, Hotkey-Einstellung, README (Recapr/Notify-Namen aufräumen), Tests für neue Settings-Felder.

## Prüfung
- `cargo check` und `cargo test` nach jeder Phase (Stand vor dem Umbau: 98 Tests grün).
- Export-Test: KW30-Dokument aus Beispieldaten erzeugen und in Word öffnen; Ferienwoche und üK-Woche separat.
- Manuell: Fenster bleibt oben, stiehlt keinen Fokus, Position wird gemerkt.

## Stand 2026-10-08 (Übergabe)
- Gepusht: `6d6335f` auf Snipy-Tools/Notify (Journal, Wochenplan, Export, Theme, Pille/Panel, Popup-Einstellungen).
- Lokal, nicht committet: Claude-Limits-Ring (src/limits.rs, ui/limits.html, pill.html, settings.*). Datenquelle: `%APPDATA%\notify\claude-limits.json`, geschrieben von `C:\Privat\Projekte\Recapr\hooks\statusline.js`.
- Offen (Agent lief beim Herunterfahren): Einstellungen für Ringfarben (5h, Woche, Kontext, Warnfarbe, Kritisch-Farbe; Hex-Feld + Swatches, KEIN nativer Farbdialog, da das Popup bei Fokusverlust schliesst) und für die Schwellen (Warnung ab 80 %, Kritisch ab 95 %, warn < kritisch, 1..=100). Wirkt live auf Pille und Detailkarte.
- Danach: `cargo test --offline`, README um Claude-Limits/statusline.js ergänzen, committen (ohne Co-Autor, Autor JM2101), pushen.
- Sicherungen: Notify.bak-2026-10-08, Notify.bak-phase3 (neben dem Repo).
