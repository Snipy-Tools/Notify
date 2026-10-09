# Notify: Arbeitsjournal (ABJ)

Notify ist eine kleine Windows-Tray-App (Rust, tao + wry). Eine schmale Leiste bleibt im Vordergrund, du trägst
Tätigkeiten mit Dauer ein, die App erinnert während der Arbeitszeit daran, sammelt deine Git-Commits und erzeugt
am Ende der Woche das Arbeitsjournal als Word-Datei aus einer Vorlage.
Alles läuft lokal: kein Netzwerk, keine Telemetrie, keine KI. Nur Windows.

## Bedienen

**Leiste.** Zwei rahmenlose, runde Fenster, immer im Vordergrund: die **Pille** (400×64) und, ausgeklappt, das **Panel** (400×560)
mit 24 px Abstand daneben. Beide Fenster sind wirklich rund (transparenter WebView plus runde Fensterregion), es gibt keine eckigen Kanten.
Pille und Zustand werden gemerkt. Die Leiste nimmt beim Einblenden keinen Fokus, ausser du öffnest sie per Tastenkürzel oder Menü.

- **Pille:** Icon in der Farbe des Ortes, Datum und Ort, Fortschrittsbalken mit `6.4 / 8.4 h`. Soll erreicht: Häkchen und grün, über Soll:
  Pfeil und `+0.8 h zu viel`. üK, Ferien und ein Gibb-Tag ohne Stunden zeigen `Keine Erinnerungen heute`. Rechts der Ein-/Ausklapp-Knopf
  (der Pfeil zeigt, wohin das Panel klappt); ausgeklappt steht links davon das **Zahnrad** für die Einstellungen. Ziehen an der Pille verschiebt
  die Leiste (Panel folgt, rastet am Rand ein).
- **Aufklapprichtung** (Einstellungen, Allgemein): *Automatisch* (Standard) klappt dorthin, wo auf dem Monitor der Pille mehr Platz bis zum Rand ist
  (unten mehr Platz: Pille oben, Panel unten; sonst Panel oben, Pille unten), *Unten* oder *Oben* erzwingen eine Richtung. Die Pille bleibt beim
  Aufklappen stehen; nur wenn das Panel sonst nicht auf den Monitor passt, rückt sie gerade so weit, dass alles passt, und bleibt dort.
- **Panel:** Titel `Montag, 05.10.` mit Heute-Badge, Pfeile wechseln den Tag (nicht über heute hinaus, `Zu heute` springt zurück), Kalender-Knopf öffnet die Woche.
  Der Ort des Tages ist eine Segmentleiste (`Noser`, `Gibb`, `üK`, `Ferien`, lange Namen zählen mit dem ersten Wort, der volle Name steht im Tooltip).
  Der Summenblock zeigt `6.4 von 8.4 h` mit Balken und `noch 2.0 h`, `Soll erreicht` oder `+0.8 h zu viel`. Darunter die Einträge, unten die Eingabezeile.
- Eingabezeile: `Text` und `Dauer`. Enter speichert. Die Dauer darf auch am Textende stehen (`Bug gefixt 2.4`, `2,4h`).
  Eingetragen wird beim angezeigten Tag.
- Klick auf einen Eintrag bearbeitet ihn (Text und Dauer am Ende), das Papierkorb-Symbol löscht eigene Einträge.
  Commits lassen sich nicht löschen, sonst importiert Git sie neu; sie bekommen ihre Dauer über das Bearbeiten.
- `Rest auf letzte Zeile buchen` schreibt die fehlende Zeit auf die letzte Zeile des Tages.
- Esc in der Eingabe blendet die Leiste aus. Der Knopf rechts in der Pille klappt das Panel ein und aus.

**Tastenkürzel** (Standard `Strg+Alt+J`, in den Einstellungen änderbar): blendet die Leiste ein und fokussiert die Eingabe,
mit Fokus blendet es sie wieder aus. Ist das Kürzel belegt, steht das im Log und die App läuft weiter.

**Wochenansicht** (Tray-Menü oder Kalender-Symbol in der Leiste): Wochennavigation, fünf Tageskarten mit Ort-Dropdown und Summe,
**Ganze Woche = Ferien / üK** (und zurück auf den Wochenplan), Textfelder für Wochenrückblick, Reflexion und Stimmung
und der Export. In einer reinen Ferien- oder üK-Woche zeigen die leeren Felder in Grau die festen Texte, die der Export einsetzt.
Eigene Texte gehen immer vor.

**Tray-Menü:** Eintrag schreiben, Woche, Heute ist ... (Arbeit, Gibb, ÜK, Ferien), Woche exportieren, Letzte Woche exportieren,
Journal-Ordner öffnen, Einstellungen, Beenden.
Das Icon zeigt den Zustand: Ring = ruhig, amberfarbener Ring = Eintrag fällig, gefüllt = gespeichert.

**Einstellungen** öffnen als rundes Popover neben der Leiste (Zahnrad in der Pille oder Tray-Menü, bei eingeklappter Leiste an der Pille). Es liegt
ganz auf dem Monitor und verdeckt Pille und Panel nicht (bevorzugt rechts oder links daneben, sonst darunter oder darüber). Es schliesst bei Esc,
*Abbrechen*, *Speichern*, Klick ausserhalb und nochmaligem Klick aufs Zahnrad, bleibt aber offen, solange ein Datei- oder Ordnerdialog offen ist
oder ein ungespeicherter Wert ungültig ist (dann steht der Fehler im Fenster). Tabs: *Allgemein* (Journal-Ordner, Exportordner, Tastenkürzel,
Aufklapprichtung, Autostart), *Wochenplan* (Standardort pro Wochentag, einen Ort nochmals anklicken = frei; Tagessoll pro Ort; Schalter
«Rest automatisch auffüllen», Standard aus), *Vorlage* (Nach- und Vorname, Word-Vorlage, feste Texte für Ferien und üK, Vorschau des Dateinamens),
*Erinnerungen* (Intervall, Später erinnern, Weg-Schwelle, Commit-Erinnerung, Arbeitszeiten, Reflexionstag, Git-Ordner und E-Mail-Adressen).
Ungültige Eingaben werden gemeldet und nicht still korrigiert. Die Orte lassen sich nicht umbenennen.

## Erinnerungen

- **Stündlich** (Intervall einstellbar): in jedem Arbeitsblock alle *Intervall* Minuten nach dem Blockbeginn und am Blockende,
  nur an Arbeitstagen, also nicht bei Gibb, üK und Ferien. Ist der Bildschirm gesperrt, der Rechner im Standby oder länger als die
  Weg-Schwelle keine Eingabe, wird nicht gefragt; nach der Rückkehr kommt höchstens ein Hinweis.
- **Nach einem neuen Git-Commit** (abschaltbar): nur in der Arbeitszeit an Arbeitstagen, nicht wenn du weg bist, und höchstens ein Hinweis
  pro Intervall. Der erste Scan nach dem Start löst keinen Hinweis aus.
- Der Hinweis ist ein kleines rundes Fenster (400×140) unten rechts, das keinen Fokus nimmt; steht dort die Leiste, erscheint es daneben.
  **Jetzt eintragen** öffnet die Leiste mit fokussierter Eingabe, **In 15 min** (einstellbar: *Später erinnern*) erinnert später nochmal. Am Reflexionstag (Standard Freitag) fragt der letzte Hinweis nach dem Wochenrückblick.

## Export

Der Export füllt eine Word-Vorlage (`.docx`) und schreibt `Arbeitsjournal-<nachname>-<vorname>-<jahr>-<KW>.docx` in den Exportordner
(Standard `Dokumente\Notify`); danach wird die Datei im Explorer markiert. Das Unterschriftsdatum ist der Freitag der Woche.
Ohne eigene Vorlage wird die mitgelieferte benutzt; sie lässt sich in den Einstellungen (Vorlage) in den Exportordner speichern
und als Ausgangspunkt für eine eigene verwenden. Platzhalter stehen in doppelten geschweiften Klammern, Unbekannte werden entfernt:

| Platzhalter | Inhalt |
|---|---|
| `{{NACHNAME}}`, `{{VORNAME}}`, `{{KW}}`, `{{JAHR}}`, `{{VON}}`, `{{BIS}}`, `{{UNTERSCHRIFTSDATUM}}` | Einzelwerte |
| `{{TAG}}`, `{{DATUM}}`, `{{ORT}}`, `{{TAETIGKEIT}}`, `{{ZEIT}}`, `{{TOTAL}}` | Die Tabellenzeile mit `{{TAG}}` wird pro Tag geklont |
| `{{WOCHENRUECKBLICK}}`, `{{REFLEXION}}`, `{{STIMMUNG}}` | Wochentexte, der Absatz wird pro Zeile geklont |

Ferien und üK haben keine Stunden. Eine reine Ferien- oder üK-Woche (üK gewinnt bei Mischung) bekommt Tätigkeit, Rückblick,
Reflexion und Stimmung aus den festen Texten der Einstellungen.

Kommandozeile:

```
notify --export            aktuelle Woche exportieren
notify --export --last     letzte Woche exportieren
notify --export --no-open  Explorer danach nicht öffnen
```

Exit-Code 0 bei Erfolg, 1 bei Fehler, 2 bei einer unbekannten Option. Das funktioniert auch, während die App läuft.

## Claude-Limits in der Pille

Statt des Ort-Icons zeigt die Pille drei Ringe: aussen das 5-Stunden-Limit, in der Mitte das Wochenlimit, innen den Kontext. Ein Klick öffnet eine Karte mit Prozenten und Reset-Zeiten.

- Datenquelle ist `%APPDATA%
otify\claude-limits.json`. Geschrieben wird sie von der Statuszeile `hooks\statusline.js` in diesem Projekt (in Claude Code als `statusLine` eintragen). Ist die Datei älter als 6 Stunden, gibt es keine Ringe.
- Einstellungen, Bereich "Allgemein": Schalter für die Anzeige, fünf Farben (Hex-Feld oder Farbfeld; ein Ring färbt sich bei Warnung bzw. kritisch um) und die beiden Schwellen. Standard: Warnung ab 80 %, kritisch ab 95 %. Die Warnschwelle muss unter der kritischen liegen (1 bis 100). Änderungen gelten sofort.

## Wo liegen die Daten

| Was | Wo |
|---|---|
| Journal (eine Datei pro Kalenderwoche) | `%APPDATA%\notify\weeks\2026-KW41.json` (Ordner einstellbar) |
| Alte Monatsdateien (`.jsonl`) | `%APPDATA%\notify\journal`, werden einmalig importiert und bleiben unangetastet |
| Einstellungen | `%APPDATA%\notify\settings.json` |
| Position und Zustand der Leiste | `%LOCALAPPDATA%\notify\bar` |
| Log (wird ab 512 KB rotiert) | `%LOCALAPPDATA%\notify\notify.log` |
| Export | `Dokumente\Notify` (einstellbar) |
| Autostart | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, Wert `Notify` |

Wochen-Dateien werden atomar geschrieben; eine beschädigte Datei wird nie überschrieben.
Git: Alle 5 Minuten (und wenn ein Hinweis fällig wird) werden die Repos unter den eingestellten Ordnern (bis 4 Ebenen tief) mit
`git log` nach eigenen Commits der letzten 7 Tage durchsucht. Das Programm `git` muss im PATH liegen. Doppelte Commits
(gleiches Repo, gleicher Hash) werden nicht erneut eingetragen. Ohne Ordner oder E-Mail bleibt Git aus.
Es läuft nur eine Instanz; eine zweite beendet sich still.

## Bauen

```
cargo build --release --offline
cargo test --offline
```
