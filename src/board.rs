// The board: tasks, notes, projects and stand-up facts in one SQLite file.
// The schema is Charlie Lite's, unchanged, so SCRUM_DB can point at a Charlie database.
use crate::time::{DAY, fmt_day, fmt_when, iso, next_day, parse_when, start_of_day};
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fmt, fs};

pub const STATUSES: [&str; 6] = ["backlog", "todo", "doing", "review", "blocked", "done"];
pub const NOTE_KINDS: [&str; 3] = ["note", "decision", "report"];
pub const PERIODS: [&str; 3] = ["last_workday", "today", "week"];
const LOOKBACK_DAYS: i64 = 14;

#[derive(Debug)]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl From<String> for Error {
    fn from(e: String) -> Self {
        Error(e)
    }
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error(e.to_string())
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

pub fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}

// ---------- db ----------

pub fn db_file() -> String {
    if let Ok(f) = env::var("SCRUM_DB") {
        return f;
    }
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    let data = env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    data.join("huddle/scrum.db").to_string_lossy().into()
}

// Append-only: never edit a shipped migration, add a new one.
// Migration 1 is Charlie Lite's, unchanged (reminders included), so the two can share a file.
const MIGRATIONS: [&str; 1] = ["
  CREATE TABLE projects (id INTEGER PRIMARY KEY, slug TEXT NOT NULL UNIQUE, path TEXT NOT NULL, test_cmd TEXT) STRICT;

  CREATE TABLE tasks (
    id INTEGER PRIMARY KEY,
    project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
    title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'todo' CHECK (status IN ('backlog','todo','doing','review','blocked','done')),
    priority INTEGER NOT NULL DEFAULT 2 CHECK (priority BETWEEN 0 AND 3),
    due_at INTEGER, blocked_reason TEXT,
    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, done_at INTEGER, deleted_at INTEGER
  ) STRICT;

  CREATE TABLE notes (
    id INTEGER PRIMARY KEY, project_id INTEGER REFERENCES projects(id), task_id INTEGER REFERENCES tasks(id),
    kind TEXT NOT NULL DEFAULT 'note' CHECK (kind IN ('note','decision','report')),
    content TEXT NOT NULL, created_at INTEGER NOT NULL
  ) STRICT;

  CREATE TABLE reminders (
    id INTEGER PRIMARY KEY, text TEXT NOT NULL, due_at INTEGER NOT NULL, task_id INTEGER REFERENCES tasks(id),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','fired','dismissed')), fired_at INTEGER
  ) STRICT;

  CREATE TABLE events (
    id INTEGER PRIMARY KEY, type TEXT NOT NULL, source TEXT NOT NULL, task_id INTEGER,
    payload TEXT NOT NULL DEFAULT '{}', created_at INTEGER NOT NULL
  ) STRICT;
  CREATE INDEX events_time ON events(created_at);
"];

pub fn open_db(file: &str) -> Result<Connection> {
    if file != ":memory:" {
        if let Some(dir) = Path::new(file).parent() {
            fs::create_dir_all(dir)?;
        }
    }
    let db = Connection::open(file)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (v, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        tx(&db, || Ok(db.execute_batch(&format!("{sql}\nPRAGMA user_version = {};", v + 1))?))?;
    }
    Ok(db)
}

fn tx<T>(db: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    db.execute_batch("BEGIN IMMEDIATE")?;
    match f() {
        Ok(v) => {
            db.execute_batch("COMMIT")?;
            Ok(v)
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

// ---------- tasks ----------

#[derive(Clone, Debug)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: i64,
    pub due_at: Option<i64>,
    pub blocked_reason: Option<String>,
    pub deleted_at: Option<i64>,
    pub project: Option<String>,
}

impl Task {
    pub fn view(&self) -> Value {
        let mut v = self.brief_base();
        v.insert("priority".into(), self.priority.into());
        self.extras(&mut v);
        if !self.description.is_empty() {
            v.insert("description".into(), self.description.clone().into());
        }
        Value::Object(v)
    }

    /// The short form stand-up facts use: priority only when it isn't normal, no description.
    pub fn brief(&self) -> Value {
        let mut v = self.brief_base();
        self.extras(&mut v);
        if self.priority != 2 {
            v.insert("priority".into(), self.priority.into());
        }
        if let Some(b) = v.shift_remove("blocked_reason") {
            v.insert("blocked_reason".into(), b);
        }
        Value::Object(v)
    }

    fn brief_base(&self) -> Map<String, Value> {
        let mut v = Map::new();
        v.insert("id".into(), fmt_id(self.id).into());
        v.insert("title".into(), self.title.clone().into());
        v.insert("status".into(), self.status.clone().into());
        v
    }

    fn extras(&self, v: &mut Map<String, Value>) {
        if let Some(p) = &self.project {
            v.insert("project".into(), p.clone().into());
        }
        if let Some(d) = self.due_at {
            v.insert("due".into(), fmt_when(d).into());
        }
        if let Some(b) = self.blocked_reason.as_ref().filter(|b| !b.is_empty()) {
            v.insert("blocked_reason".into(), b.clone().into());
        }
    }
}

pub fn fmt_id(id: i64) -> String {
    format!("#{id}")
}

/// "#42", "42", or a Charlie id like "O-42" → 42.
pub fn parse_id(r: &str) -> Result<i64> {
    let s = r.trim();
    let s = match s.as_bytes() {
        [c, b'-', ..] if c.is_ascii_alphabetic() => &s[2..],
        [c, ..] if c.is_ascii_alphabetic() => &s[1..],
        _ => s,
    };
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return err(format!("\"{r}\" is not a task id (looks like #12)"));
    }
    s.parse().map_err(|_| Error(format!("\"{r}\" is not a task id (looks like #12)")))
}

pub fn parse_priority(p: &str) -> Result<i64> {
    let n = match p.to_lowercase().as_str() {
        "urgent" => 0,
        "high" => 1,
        "normal" | "medium" => 2,
        "low" => 3,
        other => other.parse().unwrap_or(-1),
    };
    if !(0..=3).contains(&n) {
        return err(format!("priority must be 0-3 or urgent/high/normal/low, got \"{p}\""));
    }
    Ok(n)
}

fn check_status(s: &str) -> Result<&'static str> {
    STATUSES.iter().find(|x| **x == s).copied()
        .ok_or_else(|| Error(format!("status must be one of {}; got \"{s}\"", STATUSES.join(", "))))
}

fn log_event(db: &Connection, kind: &str, task: Option<i64>, payload: Value, now: i64) -> Result<()> {
    db.execute("INSERT INTO events (type, source, task_id, payload, created_at) VALUES (?1, 'scrum', ?2, ?3, ?4)",
        params![kind, task, payload.to_string(), now])?;
    Ok(())
}

pub fn add_project(db: &Connection, slug: &str, path: &str) -> Result<Value> {
    let slug = slug.trim().to_lowercase();
    let ok = slug.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if !ok {
        return err(format!("project slug \"{slug}\" may only use a-z, 0-9, . _ -"));
    }
    tx(db, || {
        db.execute("INSERT INTO projects (slug, path) VALUES (?1, ?2) ON CONFLICT(slug) DO UPDATE SET path = excluded.path", params![slug, path])?;
        log_event(db, "project.add", None, json!({ "slug": slug, "path": path }), crate::time::now())?;
        Ok(json!({ "slug": slug, "path": path }))
    })
}

pub fn list_projects(db: &Connection) -> Result<Vec<(String, String)>> {
    let mut q = db.prepare("SELECT slug, path FROM projects ORDER BY slug")?;
    let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn project_id(db: &Connection, slug: Option<&str>) -> Result<Option<i64>> {
    let Some(slug) = slug.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()) else { return Ok(None) };
    if let Some(id) = db.query_row("SELECT id FROM projects WHERE slug = ?1", [&slug], |r| r.get(0)).optional()? {
        return Ok(Some(id));
    }
    let known: Vec<String> = list_projects(db)?.into_iter().map(|p| p.0).collect();
    let known = if known.is_empty() { "none yet; add one with `project add`".into() } else { known.join(", ") };
    err(format!("unknown project \"{slug}\" (known: {known})"))
}

const TASK_SELECT: &str = "SELECT t.id, t.title, t.description, t.status, t.priority, t.due_at, t.blocked_reason, t.deleted_at, p.slug
  FROM tasks t LEFT JOIN projects p ON p.id = t.project_id";

fn tasks(db: &Connection, sql: &str, args: Vec<Sql>) -> Result<Vec<Task>> {
    let mut q = db.prepare(sql)?;
    let rows = q.query_map(params_from_iter(args), |r| {
        Ok(Task {
            id: r.get(0)?,
            title: r.get(1)?,
            description: r.get(2)?,
            status: r.get(3)?,
            priority: r.get(4)?,
            due_at: r.get(5)?,
            blocked_reason: r.get(6)?,
            deleted_at: r.get(7)?,
            project: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn get_row(db: &Connection, id: i64) -> Result<Task> {
    tasks(db, &format!("{TASK_SELECT} WHERE t.id = ?"), vec![Sql::Integer(id)])?.pop()
        .ok_or_else(|| Error(format!("no task {}", fmt_id(id))))
}

#[derive(Default)]
pub struct NewTask<'a> {
    pub title: &'a str,
    pub project: Option<&'a str>,
    pub due: Option<&'a str>,
    pub priority: Option<&'a str>,
    pub status: Option<&'a str>,
    pub description: Option<&'a str>,
}

pub fn add_task(db: &Connection, t: &NewTask, now: i64) -> Result<Task> {
    let title = t.title.trim();
    if title.is_empty() {
        return err("a task needs a title");
    }
    let status = check_status(t.status.filter(|s| !s.is_empty()).unwrap_or("todo"))?;
    let priority = t.priority.filter(|p| !p.is_empty()).map(parse_priority).transpose()?.unwrap_or(2);
    let due = t.due.filter(|d| !d.is_empty()).map(|d| parse_when(d, now)).transpose()?;
    tx(db, || {
        db.execute(
            "INSERT INTO tasks (project_id, title, description, status, priority, due_at, created_at, updated_at, done_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
            params![project_id(db, t.project)?, title, t.description.unwrap_or(""), status, priority, due, now, (status == "done").then_some(now)],
        )?;
        let id = db.last_insert_rowid();
        log_event(db, "task.add", Some(id), json!({ "title": title, "status": status }), now)?;
        get_row(db, id)
    })
}

/// A change to a task; `due: Some("none")` clears the due date.
#[derive(Default)]
pub struct Patch<'a> {
    pub status: Option<&'a str>,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub priority: Option<&'a str>,
    pub due: Option<&'a str>,
    pub project: Option<&'a str>,
    pub blocked_reason: Option<&'a str>,
    pub delete: bool,
    pub restore: bool,
}

pub fn update_task(db: &Connection, r: &str, p: &Patch, now: i64) -> Result<Task> {
    let id = parse_id(r)?;
    let given = |o: Option<&str>| o.is_some_and(|s| !s.is_empty());
    tx(db, || {
        let cur = get_row(db, id)?;
        if cur.deleted_at.is_some() && !p.restore {
            return err(format!("{} is deleted; restore it first", fmt_id(id)));
        }
        if p.status == Some("blocked") && !given(p.blocked_reason) && !given(cur.blocked_reason.as_deref()) {
            return err("blocking a task needs a reason");
        }
        let mut set: Vec<(&str, Sql)> = vec![];
        let mut put = |k: &'static str, v: Sql| match set.iter().position(|(x, _)| *x == k) {
            Some(i) => set[i].1 = v,
            None => set.push((k, v)),
        };
        let mut changes = Map::new();

        if let Some(t) = p.title.map(str::trim).filter(|t| !t.is_empty()) {
            put("title", Sql::Text(t.into()));
        }
        if let Some(d) = p.description {
            put("description", Sql::Text(d.into()));
        }
        if let Some(pr) = p.priority.filter(|x| !x.is_empty()) {
            put("priority", Sql::Integer(parse_priority(pr)?));
        }
        if let Some(d) = p.due {
            put("due_at", if d.is_empty() || d == "none" { Sql::Null } else { Sql::Integer(parse_when(d, now)?) });
        }
        if let Some(pj) = p.project {
            put("project_id", project_id(db, Some(pj))?.map_or(Sql::Null, Sql::Integer));
        }
        if let Some(b) = p.blocked_reason.filter(|b| !b.is_empty()) {
            put("blocked_reason", Sql::Text(b.into()));
        }
        if let Some(s) = p.status {
            let s = check_status(s)?;
            if s != cur.status {
                put("status", Sql::Text(s.into()));
                changes.insert("from".into(), cur.status.clone().into());
                changes.insert("to".into(), s.into());
                put("done_at", if s == "done" { Sql::Integer(now) } else { Sql::Null });
                if s != "blocked" {
                    put("blocked_reason", Sql::Null);
                }
            }
        }
        if p.delete {
            put("deleted_at", Sql::Integer(now));
        }
        if p.restore {
            put("deleted_at", Sql::Null);
        }
        if set.is_empty() {
            return Ok(cur);
        }

        let cols: Vec<String> = set.iter().map(|(k, _)| format!("{k} = ?")).collect();
        let mut args: Vec<Sql> = set.iter().map(|(_, v)| v.clone()).collect();
        args.extend([Sql::Integer(now), Sql::Integer(id)]);
        db.execute(&format!("UPDATE tasks SET {}, updated_at = ? WHERE id = ?", cols.join(", ")), params_from_iter(args))?;

        let kind = if p.delete { "task.delete" } else if p.restore { "task.restore" } else if changes.contains_key("to") { "task.status" } else { "task.update" };
        changes.insert("fields".into(), set.iter().map(|(k, _)| *k).collect());
        log_event(db, kind, Some(id), Value::Object(changes), now)?;
        get_row(db, id)
    })
}

/// status: a status name, "open" (default: everything not done), or "all". done_since hides tasks done before that time. Deleted tasks never show.
#[derive(Default)]
pub struct Filter<'a> {
    pub status: Option<&'a str>,
    pub project: Option<&'a str>,
    pub query: Option<&'a str>,
    pub due_before: Option<&'a str>,
    pub done_since: Option<i64>,
    pub limit: Option<i64>,
}

pub fn list_tasks(db: &Connection, f: &Filter, now: i64) -> Result<Vec<Task>> {
    let mut wh = vec!["t.deleted_at IS NULL"];
    let mut args = vec![];
    match f.status.filter(|s| !s.is_empty()).unwrap_or("open") {
        "open" => wh.push("t.status != 'done'"),
        "all" => {}
        s => {
            wh.push("t.status = ?");
            args.push(Sql::Text(check_status(s)?.into()));
        }
    }
    if let Some(id) = project_id(db, f.project)? {
        wh.push("t.project_id = ?");
        args.push(Sql::Integer(id));
    }
    if let Some(q) = f.query.filter(|q| !q.trim().is_empty()) {
        if let Ok(id) = parse_id(q) {
            wh.push("t.id = ?");
            args.push(Sql::Integer(id));
        } else {
            // shortcut: LIKE scan, move to FTS5 if tasks pass ~5k rows
            wh.push("(t.title LIKE ? OR t.description LIKE ?)");
            args.extend([Sql::Text(format!("%{q}%")), Sql::Text(format!("%{q}%"))]);
        }
    }
    if let Some(d) = f.due_before {
        wh.push("t.due_at IS NOT NULL AND t.due_at < ?");
        args.push(Sql::Integer(parse_when(d, now)?));
    }
    if let Some(t) = f.done_since {
        wh.push("(t.done_at IS NULL OR t.done_at >= ?)");
        args.push(Sql::Integer(t));
    }
    args.push(Sql::Integer(f.limit.unwrap_or(200)));
    tasks(db, &format!(
        "{TASK_SELECT} WHERE {}
         ORDER BY CASE t.status WHEN 'doing' THEN 0 WHEN 'blocked' THEN 1 WHEN 'review' THEN 2 WHEN 'todo' THEN 3 WHEN 'backlog' THEN 4 ELSE 5 END,
                  t.priority, t.due_at IS NULL, t.due_at, t.id
         LIMIT ?", wh.join(" AND ")), args)
}

pub fn add_note(db: &Connection, content: &str, kind: Option<&str>, task: Option<&str>, project: Option<&str>, now: i64) -> Result<Value> {
    let content = content.trim();
    if content.is_empty() {
        return err("a note needs content");
    }
    let kind = kind.filter(|k| !k.is_empty()).unwrap_or("note");
    if !NOTE_KINDS.contains(&kind) {
        return err(format!("note kind must be one of {}", NOTE_KINDS.join(", ")));
    }
    tx(db, || {
        let task = task.filter(|t| !t.is_empty()).map(parse_id).transpose()?;
        if let Some(id) = task {
            get_row(db, id)?;
        }
        db.execute("INSERT INTO notes (project_id, task_id, kind, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project_id(db, project)?, task, kind, content, now])?;
        let id = db.last_insert_rowid();
        log_event(db, "note.add", task, json!({ "kind": kind }), now)?;
        Ok(json!({ "id": id, "kind": kind, "saved": fmt_when(now) }))
    })
}

// ---------- stats ----------

/// Tasks finished on each of the last `days` days, oldest first.
pub fn done_per_day(db: &Connection, days: usize, now: i64) -> Result<Vec<u64>> {
    let starts = crate::time::last_days(days, now);
    let mut out = vec![0; days];
    for at in db.prepare("SELECT done_at FROM tasks WHERE deleted_at IS NULL AND done_at >= ?1")?.query_map([starts[0]], |r| r.get::<_, i64>(0))? {
        let at = at?;
        if let Some(i) = starts.iter().rposition(|s| at >= *s) {
            out[i] += 1;
        }
    }
    Ok(out)
}

pub struct ProjectStat {
    pub slug: String,
    pub open: i64,
    pub done: i64,
}

/// Open tasks, and tasks done since `since`, per registered project.
pub fn project_stats(db: &Connection, since: i64) -> Result<Vec<ProjectStat>> {
    let mut q = db.prepare(
        "SELECT p.slug, COALESCE(SUM(t.status != 'done'), 0), COALESCE(SUM(t.done_at >= ?1), 0)
         FROM projects p LEFT JOIN tasks t ON t.project_id = p.id AND t.deleted_at IS NULL GROUP BY p.id ORDER BY p.slug")?;
    let rows = q.query_map([since], |r| Ok(ProjectStat { slug: r.get(0)?, open: r.get(1)?, done: r.get(2)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

// ---------- facts ----------

pub struct Commit {
    pub project: String,
    pub hash: String,
    pub subject: String,
    pub at: i64,
}

/// Your commits in every registered project, using each repo's own user.email.
// shortcut: git runs with no timeout, add one if a repo on a dead network mount ever hangs a stand-up
pub fn commits(db: &Connection, from: i64, to: i64) -> Vec<Commit> {
    let mut out = vec![];
    for (slug, path) in list_projects(db).unwrap_or_default() {
        let git = |args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(&path).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
            o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
        };
        // project path gone, not a repo, or no email set: skip it rather than fail the stand-up
        let Some(email) = git(&["config", "user.email"]) else { continue };
        let Some(log) = git(&["log", "--all", "--no-merges", &format!("--author={}", email.trim()), &format!("--since={}", iso(from)),
            &format!("--until={}", iso(to)), "--format=%h%x09%ct%x09%s"]) else { continue };
        for line in log.lines() {
            let mut part = line.splitn(3, '\t');
            if let (Some(hash), Some(ct), Some(subject)) = (part.next(), part.next(), part.next()) {
                out.push(Commit { project: slug.clone(), hash: hash.into(), subject: subject.into(), at: ct.parse::<i64>().unwrap_or(0) * 1000 });
            }
        }
    }
    out.sort_by_key(|c| c.at);
    out
}

/// The most recent day before today with any board activity or commits; yesterday if there's no history.
pub fn last_workday(db: &Connection, now: i64) -> Result<(i64, i64)> {
    let today = start_of_day(now);
    let oldest = start_of_day(today - LOOKBACK_DAYS * DAY);
    let event: Option<i64> = db.query_row("SELECT MAX(created_at) FROM events WHERE created_at >= ?1 AND created_at < ?2", [oldest, today], |r| r.get(0))?;
    let latest = commits(db, oldest, today).iter().map(|c| c.at).max().max(event);
    let from = start_of_day(latest.unwrap_or(today - 1));
    Ok((from, next_day(from)))
}

/// Everything a stand-up, plan, end-of-day or weekly review needs, as one JSON object.
pub fn facts(db: &Connection, period: &str, now: i64) -> Result<Value> {
    if !PERIODS.contains(&period) {
        return err(format!("period must be one of {}", PERIODS.join(", ")));
    }
    let today = start_of_day(now);
    let tomorrow = next_day(today);
    let (from, to) = match period {
        "today" => (today, now),
        "week" => (start_of_day(today - 6 * DAY), now),
        _ => last_workday(db, now)?,
    };
    let open = list_tasks(db, &Filter::default(), now)?;
    let briefs = |keep: &dyn Fn(&Task) -> bool| open.iter().filter(|t| keep(t)).map(Task::brief).collect::<Value>();
    let live = |cond: &str| tasks(db, &format!("{TASK_SELECT} WHERE t.deleted_at IS NULL AND {cond}"), vec![Sql::Integer(from), Sql::Integer(to)]);

    // last status change per task in the window, ignoring moves to done (those are in `done`)
    let mut moved: Vec<(i64, String, String, String)> = vec![];
    let mut q = db.prepare("SELECT e.task_id, e.payload, t.title FROM events e JOIN tasks t ON t.id = e.task_id
        WHERE e.type = 'task.status' AND e.created_at >= ?1 AND e.created_at < ?2 AND t.deleted_at IS NULL ORDER BY e.created_at")?;
    for row in q.query_map([from, to], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
        let (id, payload, title) = row?;
        let p: Value = serde_json::from_str(&payload).unwrap_or_default();
        let (pf, pt) = (p["from"].as_str().unwrap_or("").to_string(), p["to"].as_str().unwrap_or("").to_string());
        match moved.iter_mut().find(|m| m.0 == id) {
            Some(m) => m.3 = pt,
            None => moved.push((id, title, pf, pt)),
        }
    }
    let decisions: Vec<String> = db.prepare("SELECT content FROM notes WHERE kind = 'decision' AND created_at >= ?1 AND created_at < ?2 ORDER BY created_at")?
        .query_map([from, to], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let last_report: Option<(String, i64)> = db.query_row(
        "SELECT content, created_at FROM notes WHERE kind = 'report' AND created_at < ?1 ORDER BY created_at DESC LIMIT 1",
        [if period == "week" { now } else { today }], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;

    Ok(json!({
        "period": period,
        "window": {
            "from": iso(from),
            "to": iso(to),
            "label": if period == "week" { format!("{} – {}", fmt_day(from), fmt_day(now)) } else { fmt_day(from) },
        },
        "now": fmt_when(now),
        "done": live("t.done_at >= ?1 AND t.done_at < ?2 ORDER BY t.done_at")?.iter().map(Task::brief).collect::<Value>(),
        "added": live("t.created_at >= ?1 AND t.created_at < ?2")?.iter().map(Task::brief).collect::<Value>(),
        "moved": moved.into_iter().filter(|m| m.3 != "done" && m.2 != m.3)
            .map(|(id, title, from, to)| json!({ "id": fmt_id(id), "title": title, "from": from, "to": to })).collect::<Value>(),
        "doing": briefs(&|t| t.status == "doing"),
        "review": briefs(&|t| t.status == "review"),
        "blocked": briefs(&|t| t.status == "blocked"),
        "due_today": briefs(&|t| t.due_at.is_some_and(|d| d >= today && d < tomorrow)),
        "overdue": briefs(&|t| t.due_at.is_some_and(|d| d < today)),
        "todo_count": open.iter().filter(|t| t.status == "todo").count(),
        "decisions": decisions,
        "commits": commits(db, from, to).into_iter().map(|c| json!({ "project": c.project, "hash": c.hash, "subject": c.subject })).collect::<Value>(),
        "last_report": last_report.map(|(text, at)| json!({ "written": fmt_when(at), "text": text })),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::at;

    fn db() -> Connection {
        open_db(":memory:").unwrap()
    }
    fn add(db: &Connection, title: &str, status: Option<&str>, due: Option<&str>, now: i64) -> Task {
        add_task(db, &NewTask { title, status, due, ..Default::default() }, now).unwrap()
    }

    #[test]
    fn parse_id_accepts_hash_bare_and_charlie_ids() {
        assert_eq!(parse_id("#12").unwrap(), 12);
        assert_eq!(parse_id("12").unwrap(), 12);
        assert_eq!(parse_id("O-12").unwrap(), 12);
        assert!(parse_id("twelve").is_err());
    }

    #[test]
    fn tasks_move_through_the_board_and_blocking_needs_a_reason() {
        let db = db();
        let now = crate::time::now();
        let t = add_task(&db, &NewTask { title: "Write the API", priority: Some("high"), due: Some("2026-11-02"), ..Default::default() }, now).unwrap();
        assert_eq!(fmt_id(t.id), "#1");
        let block = |reason| Patch { status: Some("blocked"), blocked_reason: reason, ..Default::default() };
        assert!(update_task(&db, "#1", &block(None), now).unwrap_err().0.contains("reason"));
        assert_eq!(update_task(&db, "1", &block(Some("waiting on keys")), now).unwrap().blocked_reason.as_deref(), Some("waiting on keys"));
        assert_eq!(update_task(&db, "#1", &Patch { status: Some("doing"), ..Default::default() }, now).unwrap().blocked_reason, None);
        update_task(&db, "#1", &Patch { status: Some("done"), ..Default::default() }, now).unwrap();
        assert_eq!(list_tasks(&db, &Filter::default(), now).unwrap().len(), 0);
        let all = Filter { status: Some("all"), ..Default::default() };
        assert_eq!(list_tasks(&db, &all, now).unwrap().len(), 1);
        update_task(&db, "#1", &Patch { delete: true, ..Default::default() }, now).unwrap();
        assert_eq!(list_tasks(&db, &all, now).unwrap().len(), 0);
    }

    #[test]
    fn events_keep_charlies_payload_shape() {
        let db = db();
        add(&db, "Ship it", None, None, 1);
        update_task(&db, "#1", &Patch { status: Some("doing"), ..Default::default() }, 2).unwrap();
        let (kind, payload): (String, String) = db.query_row("SELECT type, payload FROM events ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(kind, "task.status");
        assert_eq!(payload, r#"{"from":"todo","to":"doing","fields":["status","done_at","blocked_reason"]}"#);
    }

    #[test]
    fn done_since_hides_older_done_tasks() {
        let db = db();
        add(&db, "Old win", Some("done"), None, at("2026-10-26T10:00"));
        add(&db, "Fresh win", Some("done"), None, at("2026-10-28T10:00"));
        add(&db, "Open", None, None, at("2026-10-26T10:00"));
        let f = Filter { status: Some("all"), done_since: Some(at("2026-10-28T00:00")), ..Default::default() };
        let titles: Vec<String> = list_tasks(&db, &f, at("2026-10-28T12:00")).unwrap().into_iter().map(|t| t.title).collect();
        assert_eq!(titles, ["Open", "Fresh win"]);
    }

    #[test]
    fn stats_count_done_per_day_and_per_project() {
        let db = db();
        add_project(&db, "web", "/tmp/web").unwrap();
        add_project(&db, "api", "/tmp/api").unwrap();
        let mon = at("2026-10-26T10:00");
        let wed = at("2026-10-28T10:00");
        add_task(&db, &NewTask { title: "A", status: Some("done"), project: Some("web"), ..Default::default() }, mon).unwrap();
        add_task(&db, &NewTask { title: "B", status: Some("done"), project: Some("web"), ..Default::default() }, wed).unwrap();
        add_task(&db, &NewTask { title: "C", project: Some("web"), ..Default::default() }, wed).unwrap();
        add(&db, "D", Some("done"), None, wed);
        add(&db, "Too old", Some("done"), None, at("2026-10-01T10:00"));

        assert_eq!(done_per_day(&db, 3, wed).unwrap(), [1, 0, 2]); // Mon, Tue, Wed
        let p = project_stats(&db, at("2026-10-27T00:00")).unwrap();
        assert_eq!(p.iter().map(|p| (p.slug.as_str(), p.open, p.done)).collect::<Vec<_>>(), [("api", 0, 0), ("web", 1, 1)]);
    }

    #[test]
    fn facts_yesterday_is_the_last_day_with_activity_today_has_doing_due_overdue_blocked() {
        let db = db();
        let mon = at("2026-10-26T10:00");
        let wed = at("2026-10-28T09:00");
        add(&db, "Ship login", Some("doing"), None, mon);
        update_task(&db, "#1", &Patch { status: Some("done"), ..Default::default() }, mon + 3_600_000).unwrap();
        add(&db, "Fix crash", Some("doing"), None, mon);
        add(&db, "Old thing", None, Some("2026-10-27"), mon);
        add(&db, "Due today", None, Some("today"), wed);
        add(&db, "Stuck", Some("todo"), None, mon);
        update_task(&db, "#5", &Patch { status: Some("blocked"), blocked_reason: Some("no access"), ..Default::default() }, mon).unwrap();
        add_note(&db, "Mon: login shipped", Some("report"), None, None, mon + 7_200_000).unwrap();

        let titles = |v: &Value| v.as_array().unwrap().iter().map(|t| t["title"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let y = facts(&db, "last_workday", wed).unwrap();
        assert_eq!(y["window"]["from"], iso(start_of_day(mon))); // Tuesday was quiet, so Monday
        assert_eq!(titles(&y["done"]), ["Ship login"]);

        let t = facts(&db, "today", wed).unwrap();
        assert_eq!(titles(&t["doing"]), ["Fix crash"]);
        assert_eq!(titles(&t["overdue"]), ["Old thing"]);
        assert_eq!(titles(&t["due_today"]), ["Due today"]);
        assert_eq!(t["blocked"][0]["blocked_reason"], "no access");
        assert_eq!(t["last_report"]["text"], "Mon: login shipped");
    }
}
