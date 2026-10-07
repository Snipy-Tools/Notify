# Vibecoded
## This tool has no real purpose I just wanted to try someting.

# Recapr: Arbeitsjournal (ABJ)

Die Tray-App erinnert während der Arbeitszeit regelmässig daran, kurz aufzuschreiben, was man gemacht hat,
sammelt die eigenen Git-Commits und erzeugt am Ende der Woche eine Word-Datei fürs ABJ.
Alles läuft lokal: kein Netzwerk, keine Telemetrie, kein LLM. Nur Windows.

## Bedienen

Rechtsklick auf das Tray-Icon:

| Menüpunkt | Wirkung |
|---|---|
| Eintrag jetzt schreiben | Eingabefenster. Enter speichert, Shift+Enter neue Zeile, Esc schliesst. Später fragt nach 15 Minuten nochmal, Überspringen verwirft die Erinnerung. Oben stehen die Commits seit dem letzten Eintrag ("In Text übernehmen"). |
| Wochenreflexion schreiben | Wochenrückblick, Reflexion und Stimmung der Woche. Pro Woche zählt die zuletzt geschriebene. |
| Heute ist ... | Markiert den Tag als Arbeit, Schule, ÜK, Ferien oder Krank (die letzte Markierung gilt). An Schule, ÜK, Ferien und Krank gibt es keine Check-ins. |
| Woche exportieren / Letzte Woche exportieren | Schreibt `ABJ_2026-KW41.docx` nach `Dokumente\Recapr` und markiert die Datei im Explorer. |
| Heutige Einträge | Alle Einträge eines Tages, mit Tag-Wechsel. Notizen und Reflexionen sind änderbar und löschbar, Commits und Tagesmarkierungen nur sichtbar. |
| Journal-Ordner öffnen | Öffnet den Ordner mit den Journal-Dateien. |
| Einstellungen ... | Arbeitstage und -zeiten, Intervall, Weg-Schwelle, Schultage, Reflexionstag, Git-Ordner und E-Mails, Ort, Arbeitszeit pro Tag, Exportordner, Autostart. Ungültige Eingaben werden gemeldet und nicht still korrigiert. |

Das Icon zeigt den Zustand: grüner Ring = ruhig, amberfarbener Ring = Eintrag fällig, gefüllt = gespeichert.
Das Tastenkürzel **Strg+Alt+N** öffnet eine Schnellnotiz (ist es belegt, steht das im Log und die App läuft weiter).

Wann ein Check-in fällig wird: in jedem Arbeitsblock alle *Intervall* Minuten nach dem Blockbeginn und am Blockende,
nur an Arbeitstagen. Ist der Bildschirm gesperrt, der Rechner im Standby oder länger als die Weg-Schwelle keine Eingabe,
wird nicht gefragt. Nach der Rückkehr kommt höchstens ein Check-in. Am Reflexionstag (Standard Freitag) fragt der
letzte Check-in des Tages nach der Wochenreflexion.

### Kommandozeile

```
notify --export            aktuelle Woche exportieren
notify --export --last     letzte Woche exportieren
notify --export --no-open  Explorer danach nicht öffnen
```

Der Pfad der Datei wird ausgegeben. Exit-Code 0 bei Erfolg, 1 bei Fehler, 2 bei einer unbekannten Option.

### Zeit pro Eintrag

Die Arbeitszeit pro Tag (Standard 8,4 Stunden) wird im Verhältnis der gemessenen Zeit zwischen den Check-ins
(ohne Pause zwischen den Arbeitsblöcken) auf die Notizen des Tages verteilt, auf 0,1 Stunden genau, die Summe stimmt immer.
Bei 0 zählt nur die gemessene Zeit. Erlaubt sind 0 bis 24 Stunden. Schule, ÜK, Ferien und Krank bekommen keine Zeit.

## Wo liegen die Daten

| Was | Wo |
|---|---|
| Journal (eine Datei pro Monat, JSON Lines) | `%APPDATA%\notify\journal\2026-10.jsonl` |
| Einstellungen | `%APPDATA%\notify\settings.json` |
| Log (wird ab 512 KB rotiert) | `%LOCALAPPDATA%\notify\notify.log` |
| Export | `Dokumente\Recapr` (einstellbar) |
| Autostart | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, Wert `Notify` |

Das Journal wird nur angehängt. Kaputte Zeilen werden beim Lesen übersprungen und im Log vermerkt, sie bringen die App nicht zum Absturz.
Eintragstypen (Feld `type`): `note`, `commit`, `day` (`arbeit`, `schule`, `uek`, `ferien`, `krank`) und `reflection`.

Git: Alle 5 Minuten (und wenn ein Eintrag fällig wird) werden die Repos unter den eingestellten Ordnern (bis 4 Ebenen tief)
mit `git log` nach eigenen Commits der letzten 7 Tage durchsucht. Das Programm `git` muss im PATH liegen.
Doppelte Commits (gleiches Repo und gleicher Hash) werden nicht erneut eingetragen. Ohne Ordner oder E-Mail bleibt Git aus.

## Bewusst weggelassen oder vereinfacht

- **Eigene Vorlage für den Export:** entfällt. Der Export schreibt ein `.docx` im festen Layout der ABJ-Vorlage,
  eine Markdown-Vorlage gibt es nicht. Eine eigene Word-Vorlage mit Platzhaltern wäre ein eigener Schritt.
- **`templates/liste.md`:** nicht umgesetzt, es lag keine Vorlage vor.
- **Windows-Benachrichtigung (Toast):** keine. Fällig zeigt nur das Tray-Icon (Amber, Tooltip). Das Eingabefenster geht nie von selbst auf.
  Deshalb gibt es auch keine Einstellung "Benachrichtigung an/aus".
- **Tabellenverzeichnis:** ohne Seitenzahlen.
- **Später bei der letzten Erinnerung des Tages:** läuft die Pause nach Feierabend ab, verfällt sie.
- **Tastenkürzel:** fest auf Strg+Alt+N, nicht einstellbar.
- **Reflexion:** eine manuell geschriebene Reflexion erzeugt einen neuen Eintrag und ändert keinen bestehenden. Pro Woche zählt die letzte.
- **Git:** festes Zeitfenster von 7 Tagen, alle lokalen und Remote-Branches, nur der Betreff eines Commits (höchstens 500 Zeichen).
  Commits lassen sich in der App nicht löschen, sonst würde der Import sie wieder eintragen.
- **Tagesmarkierungen** in "Heutige Einträge" sind nur sichtbar, nicht änderbar. Man markiert den Tag einfach neu.
- **Einstellungen:** eine Änderung setzt den Timer neu (eine laufende Pause oder Erinnerung geht verloren).
  Das Tastenkürzel, die Mindestgrösse der Fenster und die Suchtiefe der Repos sind nicht einstellbar.
- **Zeitzonen und Sommerzeit:** Wochen und Tage gelten in der lokalen Zeit des Rechners, Zeitumstellungen werden nicht gesondert behandelt.
- **Mehrere Instanzen:** wie bisher läuft nur eine App. Der Export von der Kommandozeile funktioniert auch, während sie läuft.
