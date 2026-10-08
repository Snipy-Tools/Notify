use crate::journal::{Entry, Quelle};
use crate::log;
use chrono::{DateTime, Days, NaiveDate, Utc};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const LOOKBACK_DAYS: u64 = 7;
const MAX_DEPTH: usize = 4;
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SKIP_DIRS: [&str; 4] = ["node_modules", "target", "__pycache__", "venv"];
const MAX_SUBJECT: usize = 500;

static MISSING_LOGGED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, PartialEq)]
pub struct GitConfig {
    pub folders: Vec<PathBuf>,
    pub emails: Vec<String>,
}

impl GitConfig {
    /// Ohne Ordner oder E-Mail bleibt Git aus
    pub fn enabled(&self) -> bool {
        !self.folders.is_empty() && !self.emails.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub repo: String,
    pub hash: String,
    pub t: DateTime<Utc>,
    pub text: String,
}

/// Findet Repos bis `MAX_DEPTH` Ebenen unter den Ordnern. In einem Repo wird nicht weitergesucht.
pub fn find_repos(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut repos = Vec::new();
    for root in roots {
        walk(root, 0, &mut repos);
    }
    repos.sort();
    repos.dedup();
    repos
}

fn walk(dir: &Path, depth: usize, repos: &mut Vec<PathBuf>) {
    if dir.join(".git").exists() {
        repos.push(dir.to_path_buf());
        return;
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            if depth == 0 {
                log::warn(&format!("git: Ordner {} nicht lesbar: {e}", dir.display()));
            }
            return;
        }
    };
    for entry in entries.flatten() {
        // Symlinks und Junctions werden nicht verfolgt, damit es keine Schleifen gibt
        let Ok(kind) = entry.file_type() else { continue };
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        walk(&entry.path(), depth + 1, repos);
    }
}

fn repo_name(repo: &Path) -> String {
    repo.file_name().map_or_else(|| repo.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Liest die Ausgabe von `git log --format=%H %aE %at %s` (Felder getrennt durch 0x1f, Zeilen durch 0x1e)
/// und behält nur Commits mit einer der E-Mail-Adressen (Gross-/Kleinschreibung egal).
pub fn parse_log(repo: &str, output: &str, emails: &[String]) -> Vec<Commit> {
    let mut commits = Vec::new();
    for record in output.split('\x1e') {
        let record = record.trim_matches(['\n', '\r']);
        if record.is_empty() {
            continue;
        }
        let mut fields = record.splitn(4, '\x1f');
        let (Some(hash), Some(email), Some(ts), Some(subject)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            log::warn(&format!("git: Zeile in {repo} nicht lesbar"));
            continue;
        };
        let email = email.trim().to_lowercase();
        if !emails.iter().any(|e| e.to_lowercase() == email) {
            continue;
        }
        let Some(t) = ts.trim().parse::<i64>().ok().and_then(|s| DateTime::from_timestamp(s, 0)) else {
            log::warn(&format!("git: Zeitstempel in {repo} nicht lesbar: {ts}"));
            continue;
        };
        let text: String = subject.trim().chars().take(MAX_SUBJECT).collect();
        commits.push(Commit {
            repo: repo.to_string(),
            hash: hash.trim().to_string(),
            t,
            text: if text.is_empty() { "(ohne Nachricht)".to_string() } else { text },
        });
    }
    commits
}

/// Führt `git` mit Zeitlimit aus und gibt stdout zurück
fn run_git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.quotepath=off", "-c", "i18n.logOutputEncoding=UTF-8"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "git wurde nicht gefunden (nicht im PATH)".to_string()
            } else {
                format!("git nicht startbar: {e}")
            }
        })?;

    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                pipe.read_to_end(&mut buf).ok();
            }
            buf
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= GIT_TIMEOUT => {
                child.kill().ok();
                child.wait().ok();
                return Err("git hat nicht rechtzeitig geantwortet".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("git: {e}")),
        }
    };

    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if !status.success() {
        let msg = String::from_utf8_lossy(&stderr);
        return Err(msg.lines().next().unwrap_or("unbekannter Fehler").to_string());
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

pub fn repo_commits(repo: &Path, since: DateTime<Utc>, emails: &[String]) -> Result<Vec<Commit>, String> {
    let since = format!("--since={}", since.format("%Y-%m-%dT%H:%M:%SZ"));
    let output = run_git(
        repo,
        &["log", "--branches", "--remotes", &since, "--format=%H%x1f%aE%x1f%at%x1f%s%x1e"],
    )?;
    Ok(parse_log(&repo_name(repo), &output, emails))
}

/// Sammelt die Commits aller Repos seit `since`. Fehler einzelner Repos werden geloggt.
pub fn scan(config: &GitConfig, since: DateTime<Utc>) -> Vec<Commit> {
    let mut all: Vec<Commit> = Vec::new();
    let mut seen = HashSet::new();
    for repo in find_repos(&config.folders) {
        match repo_commits(&repo, since, &config.emails) {
            Ok(commits) => {
                for c in commits {
                    if seen.insert((c.repo.clone(), c.hash.clone())) {
                        all.push(c);
                    }
                }
            }
            Err(e) if e.starts_with("git wurde nicht gefunden") => {
                if !MISSING_LOGGED.swap(true, Ordering::Relaxed) {
                    log::error(&format!("git: {e}"));
                }
                break;
            }
            Err(e) => log::warn(&format!("git: {} übersprungen: {e}", repo.display())),
        }
    }
    all.sort_by_key(|c| c.t);
    all
}

/// Nur Commits, die noch nicht im Journal stehen (Schlüssel: Repo und Hash)
pub fn new_commits(found: Vec<Commit>, existing: &[Entry]) -> Vec<Commit> {
    let mut known: HashSet<(String, String)> = existing
        .iter()
        .filter(|e| e.quelle == Quelle::Commit)
        .filter_map(|e| Some((e.repo.clone()?, e.hash.clone()?)))
        .collect();
    found.into_iter().filter(|c| known.insert((c.repo.clone(), c.hash.clone()))).collect()
}

/// Commits seit dem letzten manuellen Eintrag, die noch keine Dauer haben (also nicht übernommen sind).
/// Gibt es keinen manuellen Eintrag, zählen die Commits von `today`.
pub fn pending_commits(entries: &[Entry], today: NaiveDate) -> Vec<&Entry> {
    let last = entries.iter().filter(|e| e.quelle == Quelle::Manuell).map(|e| e.t).max();
    entries
        .iter()
        .filter(|e| e.quelle == Quelle::Commit && e.stunden == 0.0)
        .filter(|e| match last {
            Some(last) => e.t > last,
            None => e.local_date() == today,
        })
        .collect()
}

/// Beginn des Zeitfensters für den Git-Import
pub fn lookback_start(now: DateTime<Utc>) -> DateTime<Utc> {
    now - chrono::Duration::days(LOOKBACK_DAYS as i64)
}

/// Erster Tag des Bereichs, in dem nach schon importierten Commits gesucht wird
pub fn lookback_first_day(today: NaiveDate) -> NaiveDate {
    today.checked_sub_days(Days::new(LOOKBACK_DAYS + 1)).unwrap_or(today)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    fn emails() -> Vec<String> {
        vec!["me@example.com".to_string()]
    }

    fn record(hash: &str, email: &str, ts: i64, subject: &str) -> String {
        format!("{hash}\x1f{email}\x1f{ts}\x1f{subject}\x1e\n")
    }

    #[test]
    fn parses_and_filters_by_email() {
        let hash = "a".repeat(40);
        let out = [
            record(&hash, "Me@Example.com", 1_700_000_000, "erste | Nachricht"),
            record(&"b".repeat(40), "other@x.ch", 1_700_000_100, "fremd"),
            record(&"c".repeat(40), "me@example.com", 1_700_000_200, "zweite"),
        ]
        .concat();
        let commits = parse_log("notify", &out, &emails());
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].repo, "notify");
        assert_eq!(commits[0].hash, hash);
        assert_eq!(commits[0].text, "erste | Nachricht");
        assert_eq!(commits[0].t.timestamp(), 1_700_000_000);
        assert_eq!(commits[1].text, "zweite");
    }

    #[test]
    fn broken_records_are_skipped() {
        let out = format!(
            "{}garbage ohne felder\x1e\n{}\x1fme@example.com\x1fnicht-zahl\x1fx\x1e\n{}",
            record(&"a".repeat(40), "me@example.com", 1_700_000_000, "ok"),
            "d".repeat(40),
            record(&"e".repeat(40), "me@example.com", 1_700_000_300, ""),
        );
        let commits = parse_log("r", &out, &emails());
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].text, "ok");
        assert_eq!(commits[1].text, "(ohne Nachricht)");
        assert!(parse_log("r", "", &emails()).is_empty());
    }

    #[test]
    fn long_subjects_are_cut() {
        let out = record(&"a".repeat(40), "me@example.com", 1_700_000_000, &"ä".repeat(900));
        assert_eq!(parse_log("r", &out, &emails())[0].text.chars().count(), MAX_SUBJECT);
    }

    #[test]
    fn config_needs_folders_and_emails() {
        let mut c = GitConfig { folders: vec![], emails: vec![] };
        assert!(!c.enabled());
        c.folders.push(PathBuf::from("C:/x"));
        assert!(!c.enabled());
        c.emails.push("a@b.ch".into());
        assert!(c.enabled());
        c.folders.clear();
        assert!(!c.enabled());
    }

    fn make_repo(root: &Path, rel: &str) -> PathBuf {
        let dir = root.join(rel);
        fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    #[test]
    fn finds_repos_a_few_levels_deep() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = make_repo(root, "a");
        let b = make_repo(root, "work/b");
        let d4 = make_repo(root, "x/y/z/w");
        make_repo(root, "x/y/z/w/v/too-deep");
        make_repo(root, "x/y/z/w/q/r");
        make_repo(root, "node_modules/lib");
        make_repo(root, ".hidden/repo");
        make_repo(root, "a/nested-in-repo");
        fs::create_dir_all(root.join("leer/ordner")).unwrap();
        // eine .git-Datei (Worktree/Submodul) zählt auch
        let wt = root.join("wt");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), "gitdir: ../elsewhere").unwrap();

        let found = find_repos(&[root.to_path_buf()]);
        assert_eq!(found, {
            let mut v = vec![a, b, d4, wt];
            v.sort();
            v
        });
    }

    #[test]
    fn root_may_be_a_repo_and_missing_roots_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_repo(tmp.path(), "self");
        let found = find_repos(&[root.clone(), tmp.path().join("gibt-es-nicht"), root.clone()]);
        assert_eq!(found, vec![root]);
    }

    fn at(day: u32, h: u32) -> DateTime<Utc> {
        Local.with_ymd_and_hms(2026, 10, day, h, 0, 0).unwrap().with_timezone(&Utc)
    }

    fn commit(repo: &str, hash: &str, t: DateTime<Utc>) -> Commit {
        Commit { repo: repo.into(), hash: hash.into(), t, text: "msg".into() }
    }

    #[test]
    fn dedupes_by_repo_and_hash() {
        let h = "f".repeat(40);
        let existing = vec![
            Entry::commit(at(7, 9), "notify", &h, "msg"),
            Entry::manuell(at(7, 9), "x", 0.0),
        ];
        let found = vec![
            commit("notify", &h, at(7, 9)),
            commit("other", &h, at(7, 9)),
            commit("notify", &"e".repeat(40), at(7, 10)),
            commit("notify", &"e".repeat(40), at(7, 10)),
        ];
        let fresh = new_commits(found, &existing);
        assert_eq!(fresh.len(), 2);
        assert_eq!(fresh[0].repo, "other");
        assert_eq!(fresh[1].hash, "e".repeat(40));
    }

    #[test]
    fn pending_commits_since_last_note() {
        let today = at(7, 12).with_timezone(&Local).date_naive();
        let h = |c: char| c.to_string().repeat(40);
        let entries = vec![
            Entry::commit(at(6, 16), "r", &h('1'), "gestern"),
            Entry::manuell(at(7, 9), "Notiz", 1.0),
            Entry::commit(at(7, 8), "r", &h('2'), "vor der Notiz"),
            Entry::commit(at(7, 10), "r", &h('3'), "nach der Notiz"),
            Entry { stunden: 0.5, ..Entry::commit(at(7, 11), "r", &h('5'), "schon übernommen") },
            Entry::commit(at(7, 11), "r", &h('4'), "noch eins"),
        ];
        let pending = pending_commits(&entries, today);
        let hashes: Vec<_> = pending
            .iter()
            .map(|e| e.hash.as_deref().and_then(|h| h.chars().next()).unwrap_or('?'))
            .collect();
        assert_eq!(hashes, ['3', '4']);
    }

    #[test]
    fn pending_commits_without_note_are_todays() {
        let today = at(7, 12).with_timezone(&Local).date_naive();
        let h = |c: char| c.to_string().repeat(40);
        let entries = vec![
            Entry::commit(at(6, 16), "r", &h('1'), "gestern"),
            Entry::commit(at(7, 8), "r", &h('2'), "heute"),
        ];
        assert_eq!(pending_commits(&entries, today).len(), 1);
        assert!(pending_commits(&[], today).is_empty());
    }

    #[test]
    fn lookback_window() {
        let now = at(7, 12);
        assert_eq!(lookback_start(now), now - chrono::Duration::days(7));
        let today = now.with_timezone(&Local).date_naive();
        assert_eq!(lookback_first_day(today), today - chrono::Duration::days(8));
    }

    fn git_available() -> bool {
        Command::new("git").arg("--version").creation_flags(CREATE_NO_WINDOW).output().is_ok()
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn scans_a_real_repo() {
        if !git_available() {
            eprintln!("git nicht installiert, Test übersprungen");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("projekt");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["-c", "user.email=Me@Example.com", "-c", "user.name=me", "commit", "-q", "--allow-empty", "-m", "eigener Commit | mit Pipe"]);
        git(&repo, &["-c", "user.email=other@x.ch", "-c", "user.name=o", "commit", "-q", "--allow-empty", "-m", "fremder Commit"]);

        let config = GitConfig { folders: vec![tmp.path().to_path_buf()], emails: emails() };
        let found = scan(&config, Utc::now() - chrono::Duration::days(1));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].repo, "projekt");
        assert_eq!(found[0].text, "eigener Commit | mit Pipe");
        assert_eq!(found[0].hash.len(), 40);

        // zu alt: nichts
        assert!(scan(&config, Utc::now() + chrono::Duration::days(1)).is_empty());
        // kein Commit im Repo und kein Repo: keine Fehler
        let empty = tmp.path().join("leer");
        fs::create_dir_all(&empty).unwrap();
        git(&empty, &["init", "-q"]);
        assert_eq!(scan(&config, Utc::now() - chrono::Duration::days(1)).len(), 1);
    }
}
