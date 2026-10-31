// Hooks that keep the board true without anyone asking: a git post-commit hook, and a summary for Claude Code's session start.
use crate::board::{self, Filter, Patch, Result, Task, err, fmt_id};
use crate::time::start_of_day;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CLOSE: [&str; 10] = ["fix", "fixes", "fixed", "close", "closes", "closed", "resolve", "resolves", "resolved", "done"];
const HOOK_LINE: &str = "huddle hook commit || true  # added by huddle: moves board tasks named in commits";

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git").arg("-C").arg(dir).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Task refs in a commit message, in order, each with whether a closing word came first ("fixes #12, #13").
pub fn refs(msg: &str) -> Vec<(i64, bool)> {
    let mut out: Vec<(i64, bool)> = vec![];
    let mut closing = false;
    for w in msg.split(|c: char| c.is_whitespace() || ",()[]".contains(c)).filter(|w| !w.is_empty()) {
        let w = w.trim_end_matches(['.', ':', ';', '!']).to_lowercase();
        match w.strip_prefix('#').and_then(|n| n.parse::<i64>().ok()) {
            Some(n) => match out.iter_mut().find(|r| r.0 == n) {
                Some(r) => r.1 |= closing,
                None => out.push((n, closing)),
            },
            None => closing = CLOSE.contains(&w.as_str()) || (closing && w == "and"),
        }
    }
    out
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The registered project whose path is this repo's top folder.
fn project_for(db: &Connection, dir: &Path) -> Result<Option<String>> {
    let Some(top) = git(dir, &["rev-parse", "--show-toplevel"]) else { return Ok(None) };
    Ok(board::list_projects(db)?.into_iter().find(|(_, p)| same_path(Path::new(p), Path::new(&top))).map(|p| p.0))
}

/// Runs after a commit: "#12" starts that task (backlog/todo → doing), "fixes #12" finishes it.
/// Only tasks of the project registered for this repo move, so GitHub issue numbers elsewhere touch nothing.
pub fn on_commit(db: &Connection, dir: &Path, now: i64) -> Result<Vec<Value>> {
    let Some(slug) = project_for(db, dir)? else { return Ok(vec![]) };
    let msg = git(dir, &["log", "-1", "--format=%B"]).unwrap_or_default();
    let hash = git(dir, &["log", "-1", "--format=%h"]).unwrap_or_default();
    let mut moved = vec![];
    for (id, closes) in refs(&msg) {
        let f = Filter { status: Some("all"), project: Some(&slug), query: Some(&id.to_string()), ..Default::default() };
        let Some(t) = board::list_tasks(db, &f, now)?.pop() else { continue };
        let to = match (closes, t.status.as_str()) {
            (_, "done") => continue,
            (true, _) => "done",
            (false, "backlog" | "todo") => "doing",
            _ => continue,
        };
        board::update_task(db, &id.to_string(), &Patch { status: Some(to), ..Default::default() }, now)?;
        moved.push(json!({ "id": fmt_id(id), "title": t.title, "from": t.status, "to": to, "commit": hash }));
    }
    Ok(moved)
}

/// Adds the post-commit hook to a repo (respecting core.hooksPath); says what it did.
pub fn install_commit_hook(dir: &Path) -> Result<&'static str> {
    let Some(top) = git(dir, &["rev-parse", "--show-toplevel"]) else { return Ok("not a git repo") };
    let hooks = match git(dir, &["config", "core.hooksPath"]) {
        Some(p) => Path::new(&top).join(p),
        None => PathBuf::from(&top).join(git(dir, &["rev-parse", "--git-path", "hooks"]).unwrap_or(".git/hooks".into())),
    };
    let file = hooks.join("post-commit");
    let said = match fs::read_to_string(&file) {
        Ok(s) if s.contains("huddle hook commit") => return Ok("already installed"),
        Ok(s) => {
            fs::write(&file, format!("{}\n{HOOK_LINE}\n", s.trim_end()))?;
            "added to the existing post-commit hook"
        }
        Err(_) => {
            fs::create_dir_all(&hooks)?;
            fs::write(&file, format!("#!/bin/sh\n{HOOK_LINE}\n"))?;
            "installed"
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755))?;
    }
    Ok(said)
}

/// A few lines for Claude at session start: what's in flight, what's stuck, and which project this folder is.
pub fn session_summary(db: &Connection, dir: &Path, now: i64) -> Result<String> {
    let open = board::list_tasks(db, &Filter::default(), now)?;
    let today = start_of_day(now);
    let name = |t: &Task| format!("{} {}", fmt_id(t.id), t.title);
    let line = |label: &str, ts: Vec<String>| (!ts.is_empty()).then(|| format!("- {label}: {}", ts.join("; ")));
    let lines: Vec<String> = [
        line("doing", open.iter().filter(|t| t.status == "doing").map(name).collect()),
        line("blocked", open.iter().filter(|t| t.status == "blocked")
            .map(|t| format!("{} ({})", name(t), t.blocked_reason.as_deref().unwrap_or(""))).collect()),
        line("review", open.iter().filter(|t| t.status == "review").map(name).collect()),
        line("overdue", open.iter().filter(|t| t.due_at.is_some_and(|d| d < today)).map(name).collect()),
    ].into_iter().flatten().collect();
    let here = match project_for(db, dir)? {
        Some(slug) => format!("This folder is the huddle project `{slug}`."),
        None if git(dir, &["rev-parse", "--show-toplevel"]).is_some() => "This git repo isn't a huddle project yet; offer once to register it.".into(),
        None => String::new(),
    };
    if lines.is_empty() && here.is_empty() {
        return Ok(String::new());
    }
    Ok(format!("Huddle board ({} open):\n{}\n{here}\nKeep it true: move a task when work on it starts, finishes or gets blocked.",
        open.len(), lines.join("\n")).replace("\n\n", "\n"))
}

/// Registers a project and, unless asked not to, adds the commit hook to its repo.
pub fn add_project(db: &Connection, slug: &str, path: &Path, hook: bool) -> Result<Value> {
    let mut p = board::add_project(db, slug, &path.to_string_lossy())?;
    p["hook"] = (if hook { install_commit_hook(path)? } else { "skipped" }).into();
    Ok(p)
}

pub fn run(db: &Connection, which: Option<&str>, now: i64) -> Result<String> {
    let dir = std::env::current_dir()?;
    match which {
        Some("commit") => Ok(on_commit(db, &dir, now)?.iter()
            .map(|m| format!("huddle: {} {} → {}", m["id"].as_str().unwrap_or(""), m["title"].as_str().unwrap_or(""), m["to"].as_str().unwrap_or("")))
            .collect::<Vec<_>>().join("\n")),
        Some("session") => session_summary(db, &dir, now),
        _ => err("hook commit|session"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{NewTask, add_project, add_task, open_db};

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("huddle-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for args in [&["init", "-q"][..], &["config", "user.email", "t@example.com"], &["config", "user.name", "T"]] {
            git(&dir, args).unwrap();
        }
        dir
    }
    fn commit(dir: &Path, msg: &str) {
        git(dir, &["commit", "-q", "--allow-empty", "-m", msg]).unwrap();
    }
    fn status(db: &Connection, id: &str) -> String {
        board::list_tasks(db, &Filter { status: Some("all"), query: Some(id), ..Default::default() }, 0).unwrap().remove(0).status
    }

    #[test]
    fn refs_read_mentions_and_closing_words() {
        assert_eq!(refs("Start #3"), [(3, false)]);
        assert_eq!(refs("Fixes #12, #13 and #14; see #2."), [(12, true), (13, true), (14, true), (2, false)]);
        assert_eq!(refs("wip on #4 (closes #5)"), [(4, false), (5, true)]);
        assert_eq!(refs("PR#9 and issue 9"), []);
    }

    #[test]
    fn commits_move_only_this_repos_tasks_and_the_hook_installs_once() {
        let dir = repo("commit");
        let db = open_db(":memory:").unwrap();
        add_project(&db, "web", &dir.to_string_lossy()).unwrap();
        add_project(&db, "other", "/nowhere").unwrap();
        add_task(&db, &NewTask { title: "Login page", project: Some("web"), ..Default::default() }, 1).unwrap();
        add_task(&db, &NewTask { title: "Elsewhere", project: Some("other"), ..Default::default() }, 1).unwrap();

        commit(&dir, "Start the login page #1, touch #2");
        assert_eq!(on_commit(&db, &dir, 2).unwrap().len(), 1);
        assert_eq!((status(&db, "1"), status(&db, "2")), ("doing".into(), "todo".into()));
        commit(&dir, "Fixes #1");
        assert_eq!(on_commit(&db, &dir, 3).unwrap()[0]["to"], "done");
        assert_eq!(on_commit(&db, &dir, 4).unwrap().len(), 0); // already done

        assert_eq!(install_commit_hook(&dir).unwrap(), "installed");
        assert_eq!(install_commit_hook(&dir).unwrap(), "already installed");
        assert!(fs::read_to_string(dir.join(".git/hooks/post-commit")).unwrap().starts_with("#!/bin/sh\nhuddle hook commit"));
        assert!(session_summary(&db, &dir, 5).unwrap().contains("project `web`"));
        assert_eq!(install_commit_hook(&std::env::temp_dir()).unwrap(), "not a git repo");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn session_summary_names_what_is_in_flight() {
        let db = open_db(":memory:").unwrap();
        let dir = std::env::temp_dir();
        assert_eq!(session_summary(&db, &dir, 0).unwrap(), "");
        add_task(&db, &NewTask { title: "Ship it", status: Some("doing"), ..Default::default() }, 1).unwrap();
        add_task(&db, &NewTask { title: "Keys", ..Default::default() }, 1).unwrap();
        board::update_task(&db, "2", &Patch { status: Some("blocked"), blocked_reason: Some("no AWS keys"), ..Default::default() }, 1).unwrap();
        let s = session_summary(&db, &dir, 2).unwrap();
        assert!(s.contains("- doing: #1 Ship it") && s.contains("- blocked: #2 Keys (no AWS keys)"), "{s}");
    }
}
