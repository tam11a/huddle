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
    if let Some(f) = env::var("SCRUM_DB").ok().filter(|f| !f.is_empty()) {
        return f;
    }
    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    let data = env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    data.join("huddle/scrum.db").to_string_lossy().into()
}

// Append-only: never edit a shipped migration, add a new one.
// Migration 1 is Charlie Lite's, unchanged (reminders included); migration 2 adds sprints, points, subtasks,
// recurring tasks, tags and time tracking, which only Huddle knows about.
const MIGRATIONS: [&str; 2] = ["
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
", "
  CREATE TABLE sprints (
    id INTEGER PRIMARY KEY, project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
    name TEXT NOT NULL, goal TEXT NOT NULL DEFAULT '',
    starts_at INTEGER NOT NULL, ends_at INTEGER NOT NULL, closed_at INTEGER, created_at INTEGER NOT NULL
  ) STRICT;
  ALTER TABLE tasks ADD COLUMN sprint_id INTEGER REFERENCES sprints(id) ON DELETE SET NULL;
  ALTER TABLE tasks ADD COLUMN points INTEGER CHECK (points >= 0);
  ALTER TABLE tasks ADD COLUMN parent_id INTEGER REFERENCES tasks(id) ON DELETE SET NULL;
  ALTER TABLE tasks ADD COLUMN recur TEXT CHECK (recur IN ('daily','weekdays','weekly','monthly'));
  CREATE INDEX tasks_sprint ON tasks(sprint_id);
  CREATE INDEX tasks_parent ON tasks(parent_id);

  CREATE TABLE tags (task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE, tag TEXT NOT NULL, PRIMARY KEY (task_id, tag)) STRICT;

  CREATE TABLE time_entries (
    id INTEGER PRIMARY KEY, task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    started_at INTEGER NOT NULL, ended_at INTEGER
  ) STRICT;
  CREATE INDEX time_task ON time_entries(task_id);
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
    pub sprint_id: Option<i64>,
    pub sprint: Option<String>,
    pub points: Option<i64>,
    pub parent_id: Option<i64>,
    pub recur: Option<String>,
    pub tags: Vec<String>,
    /// (done, total) subtasks
    pub subtasks: (i64, i64),
    /// Tracked time in ms, a running timer counted up to now.
    pub spent: i64,
    /// The next copy of a recurring task, when finishing this one just created it.
    pub next: Option<i64>,
}

pub fn fmt_minutes(ms: i64) -> String {
    let m = ms / 60_000;
    if m >= 60 { format!("{}h{:02}", m / 60, m % 60) } else { format!("{m}m") }
}

impl Task {
    pub fn view(&self) -> Value {
        let mut v = self.brief_base();
        v.insert("priority".into(), self.priority.into());
        self.extras(&mut v);
        let mut put = |k: &str, x: Value| v.insert(k.into(), x);
        if let Some(s) = &self.sprint {
            put("sprint", json!(format!("{s} (#{})", self.sprint_id.unwrap_or(0))));
        }
        if let Some(p) = self.points {
            put("points", p.into());
        }
        if let Some(p) = self.parent_id {
            put("parent", fmt_id(p).into());
        }
        if let Some(r) = &self.recur {
            put("repeats", r.clone().into());
        }
        if !self.tags.is_empty() {
            put("tags", self.tags.clone().into());
        }
        if self.subtasks.1 > 0 {
            put("subtasks", format!("{}/{} done", self.subtasks.0, self.subtasks.1).into());
        }
        if self.spent > 0 {
            put("time_spent", fmt_minutes(self.spent).into());
        }
        if let Some(n) = self.next {
            put("next", fmt_id(n).into());
        }
        if !self.description.is_empty() {
            put("description", self.description.clone().into());
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

const TASK_SELECT: &str = "SELECT t.id, t.title, t.description, t.status, t.priority, t.due_at, t.blocked_reason, t.deleted_at, p.slug,
    t.sprint_id, s.name, t.points, t.parent_id, t.recur,
    (SELECT group_concat(tag, ',') FROM (SELECT tag FROM tags WHERE task_id = t.id ORDER BY tag)),
    (SELECT COUNT(*) FROM tasks c WHERE c.parent_id = t.id AND c.deleted_at IS NULL AND c.status = 'done'),
    (SELECT COUNT(*) FROM tasks c WHERE c.parent_id = t.id AND c.deleted_at IS NULL),
    (SELECT COALESCE(SUM(COALESCE(e.ended_at, CAST(unixepoch('subsec') * 1000 AS INTEGER)) - e.started_at), 0) FROM time_entries e WHERE e.task_id = t.id)
  FROM tasks t LEFT JOIN projects p ON p.id = t.project_id LEFT JOIN sprints s ON s.id = t.sprint_id";

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
            sprint_id: r.get(9)?,
            sprint: r.get(10)?,
            points: r.get(11)?,
            parent_id: r.get(12)?,
            recur: r.get(13)?,
            tags: r.get::<_, Option<String>>(14)?.map(|t| t.split(',').map(String::from).collect()).unwrap_or_default(),
            subtasks: (r.get(15)?, r.get(16)?),
            spent: r.get(17)?,
            next: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

const RECUR: [&str; 4] = ["daily", "weekdays", "weekly", "monthly"];

/// "daily", "weekdays", "weekly", "monthly"; "none" or "" clears.
fn check_recur(r: &str) -> Result<Option<&'static str>> {
    match r.trim().to_lowercase().as_str() {
        "" | "none" => Ok(None),
        r => RECUR.iter().find(|x| **x == r).copied().map(Some)
            .ok_or_else(|| Error(format!("repeat must be one of {} or none; got \"{r}\"", RECUR.join(", ")))),
    }
}

/// Story points: a whole number, or "none" / "" to clear.
fn parse_points(p: &str) -> Result<Option<i64>> {
    match p.trim() {
        "" | "none" => Ok(None),
        p => p.parse::<i64>().ok().filter(|n| *n >= 0).map(Some).ok_or_else(|| Error(format!("points must be a whole number, got \"{p}\""))),
    }
}

/// "a, B ,c" → ["a", "b", "c"], deduplicated.
fn parse_tags(t: &str) -> Vec<String> {
    let mut tags: Vec<String> = t.split(',').map(|x| x.trim().trim_start_matches('#').to_lowercase()).filter(|x| !x.is_empty()).collect();
    tags.sort();
    tags.dedup();
    tags
}

fn set_tags(db: &Connection, id: i64, tags: &[String]) -> Result<()> {
    db.execute("DELETE FROM tags WHERE task_id = ?1", [id])?;
    for t in tags {
        db.execute("INSERT INTO tags (task_id, tag) VALUES (?1, ?2)", params![id, t])?;
    }
    Ok(())
}

/// A sprint by id ("3", "#3"), "current" (the open sprint running now, in the project when one is given), or "none".
fn sprint_ref(db: &Connection, r: &str, project: Option<i64>, now: i64) -> Result<Option<i64>> {
    match r.trim() {
        "" | "none" => Ok(None),
        "current" => db.query_row(
            "SELECT id FROM sprints WHERE closed_at IS NULL AND starts_at <= ?1 AND ends_at > ?1 AND (?2 IS NULL OR project_id IS ?2 OR project_id IS NULL)
             ORDER BY project_id IS NULL, starts_at DESC LIMIT 1", params![now, project], |r| r.get(0)).optional()?
            .map(Some).ok_or_else(|| Error("no sprint is running now; start one with `sprint add`".into())),
        r => {
            let id = parse_id(r)?;
            db.query_row("SELECT id FROM sprints WHERE id = ?1", [id], |r| r.get(0)).optional()?
                .map(Some).ok_or_else(|| Error(format!("no sprint {}", fmt_id(id))))
        }
    }
}

fn parent_ref(db: &Connection, r: &str) -> Result<Option<i64>> {
    match r.trim() {
        "" | "none" => Ok(None),
        r => Ok(Some(get_row(db, parse_id(r)?)?.id)),
    }
}

/// When a repeating task is due next, after `from`: keeps the time of day, and never lands in the past.
pub fn next_due(rule: &str, from: i64, now: i64) -> i64 {
    use chrono::{DateTime, Datelike, Duration as D, Months};
    let mut t = DateTime::from_timestamp_millis(from).unwrap_or_default().with_timezone(&chrono::Local).naive_local();
    loop {
        t = match rule {
            "daily" => t + D::days(1),
            "weekdays" => {
                let mut n = t + D::days(1);
                while n.weekday().num_days_from_monday() >= 5 {
                    n += D::days(1);
                }
                n
            }
            "weekly" => t + D::days(7),
            _ => t.checked_add_months(Months::new(1)).unwrap_or(t + D::days(30)),
        };
        let ms = crate::time::at_local(t);
        if ms > now {
            return ms;
        }
    }
}

/// One task by id, deleted ones included.
pub fn get_task(db: &Connection, id: i64) -> Result<Task> {
    get_row(db, id)
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
    /// a sprint id, "current" or "none"
    pub sprint: Option<&'a str>,
    pub points: Option<&'a str>,
    /// the parent task, for a subtask
    pub parent: Option<&'a str>,
    /// daily, weekdays, weekly or monthly
    pub recur: Option<&'a str>,
    /// comma-separated
    pub tags: Option<&'a str>,
}

pub fn add_task(db: &Connection, t: &NewTask, now: i64) -> Result<Task> {
    let title = t.title.trim();
    if title.is_empty() {
        return err("a task needs a title");
    }
    let status = check_status(t.status.filter(|s| !s.is_empty()).unwrap_or("todo"))?;
    let priority = t.priority.filter(|p| !p.is_empty()).map(parse_priority).transpose()?.unwrap_or(2);
    let due = t.due.filter(|d| !d.is_empty()).map(|d| parse_when(d, now)).transpose()?;
    let points = t.points.map(parse_points).transpose()?.flatten();
    let recur = t.recur.map(check_recur).transpose()?.flatten();
    tx(db, || {
        let project = project_id(db, t.project)?;
        let parent = t.parent.map(|p| parent_ref(db, p)).transpose()?.flatten();
        let sprint = t.sprint.map(|s| sprint_ref(db, s, project, now)).transpose()?.flatten();
        db.execute(
            "INSERT INTO tasks (project_id, title, description, status, priority, due_at, created_at, updated_at, done_at, sprint_id, points, parent_id, recur)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![project, title, t.description.unwrap_or(""), status, priority, due, now, (status == "done").then_some(now), sprint, points, parent, recur],
        )?;
        let id = db.last_insert_rowid();
        set_tags(db, id, &parse_tags(t.tags.unwrap_or("")))?;
        log_event(db, "task.add", Some(id), json!({ "title": title, "status": status }), now)?;
        get_row(db, id)
    })
}

/// Finishing a repeating task makes its next copy: same details, back in todo, due at the next turn of its rule.
fn repeat(db: &Connection, t: &Task, now: i64) -> Result<i64> {
    let rule = t.recur.as_deref().unwrap_or("daily");
    let due = next_due(rule, t.due_at.unwrap_or_else(|| start_of_day(now) + 18 * 3_600_000), now);
    db.execute(
        "INSERT INTO tasks (project_id, title, description, status, priority, due_at, created_at, updated_at, points, recur)
         SELECT project_id, title, description, 'todo', priority, ?2, ?3, ?3, points, recur FROM tasks WHERE id = ?1",
        params![t.id, due, now],
    )?;
    let id = db.last_insert_rowid();
    set_tags(db, id, &t.tags)?;
    log_event(db, "task.add", Some(id), json!({ "title": t.title, "status": "todo", "repeats": fmt_id(t.id) }), now)?;
    Ok(id)
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
    /// a sprint id, "current", or "none" to take it out
    pub sprint: Option<&'a str>,
    pub points: Option<&'a str>,
    pub parent: Option<&'a str>,
    pub recur: Option<&'a str>,
    /// replaces the tags; "" clears them
    pub tags: Option<&'a str>,
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
        if let Some(s) = p.sprint {
            let project = db.query_row("SELECT project_id FROM tasks WHERE id = ?1", [id], |r| r.get::<_, Option<i64>>(0))?;
            put("sprint_id", sprint_ref(db, s, project, now)?.map_or(Sql::Null, Sql::Integer));
        }
        if let Some(pts) = p.points {
            put("points", parse_points(pts)?.map_or(Sql::Null, Sql::Integer));
        }
        if let Some(pa) = p.parent {
            let parent = parent_ref(db, pa)?;
            if parent == Some(id) {
                return err("a task can't be its own parent");
            }
            put("parent_id", parent.map_or(Sql::Null, Sql::Integer));
        }
        if let Some(r) = p.recur {
            put("recur", check_recur(r)?.map_or(Sql::Null, |r| Sql::Text(r.into())));
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
        let tags = p.tags.map(parse_tags).filter(|t| *t != cur.tags);
        if set.is_empty() && tags.is_none() {
            return Ok(cur);
        }

        let mut fields: Vec<&str> = set.iter().map(|(k, _)| *k).collect();
        if !set.is_empty() {
            let cols: Vec<String> = set.iter().map(|(k, _)| format!("{k} = ?")).collect();
            let mut args: Vec<Sql> = set.iter().map(|(_, v)| v.clone()).collect();
            args.extend([Sql::Integer(now), Sql::Integer(id)]);
            db.execute(&format!("UPDATE tasks SET {}, updated_at = ? WHERE id = ?", cols.join(", ")), params_from_iter(args))?;
        }
        if let Some(tags) = &tags {
            set_tags(db, id, tags)?;
            fields.push("tags");
        }

        let kind = if p.delete { "task.delete" } else if p.restore { "task.restore" } else if changes.contains_key("to") { "task.status" } else { "task.update" };
        let finished = changes.get("to").is_some_and(|t| t == "done");
        changes.insert("fields".into(), fields.into());
        log_event(db, kind, Some(id), Value::Object(changes), now)?;
        let mut t = get_row(db, id)?;
        if finished && t.recur.is_some() {
            t.next = Some(repeat(db, &t, now)?);
        }
        Ok(t)
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
    /// a sprint id or "current"
    pub sprint: Option<&'a str>,
    pub tag: Option<&'a str>,
    /// subtasks of this task
    pub parent: Option<&'a str>,
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
    if let Some(s) = f.sprint.filter(|s| !s.is_empty()) {
        wh.push("t.sprint_id = ?");
        args.push(sprint_ref(db, s, project_id(db, f.project)?, now)?.map_or(Sql::Null, Sql::Integer));
    }
    if let Some(tag) = f.tag.map(parse_tags).and_then(|t| t.into_iter().next()) {
        wh.push("EXISTS (SELECT 1 FROM tags g WHERE g.task_id = t.id AND g.tag = ?)");
        args.push(Sql::Text(tag));
    }
    if let Some(p) = f.parent.filter(|p| !p.is_empty()) {
        wh.push("t.parent_id = ?");
        args.push(Sql::Integer(parse_id(p)?));
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

// ---------- sprints ----------

pub fn add_sprint(db: &Connection, name: &str, project: Option<&str>, start: Option<&str>, end: Option<&str>, goal: Option<&str>, now: i64) -> Result<Value> {
    let name = name.trim();
    if name.is_empty() {
        return err("a sprint needs a name");
    }
    // a sprint starts at the beginning of its first day and ends at the end of its last
    let starts = start.filter(|s| !s.is_empty()).map(|s| parse_when(s, now)).transpose()?.map_or(start_of_day(now), start_of_day);
    let ends = match end.filter(|e| !e.is_empty()) {
        Some(e) => next_day(parse_when(e, starts)?),
        None => start_of_day(starts + 14 * DAY + 12 * 3_600_000),
    };
    if ends <= starts {
        return err("a sprint has to end after it starts");
    }
    tx(db, || {
        db.execute("INSERT INTO sprints (project_id, name, goal, starts_at, ends_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![project_id(db, project)?, name, goal.unwrap_or(""), starts, ends, now])?;
        let id = db.last_insert_rowid();
        log_event(db, "sprint.add", None, json!({ "sprint": id, "name": name }), now)?;
        sprint_detail(db, &id.to_string(), now)
    })
}

/// Progress for one sprint: tasks and points done, days left, and a burndown (remaining work at the end of each day so far).
pub fn sprint_detail(db: &Connection, r: &str, now: i64) -> Result<Value> {
    let id = sprint_ref(db, r, None, now)?.ok_or_else(|| Error("which sprint? give its id or \"current\"".into()))?;
    let (name, goal, starts, ends, closed, project): (String, String, i64, i64, Option<i64>, Option<String>) = db.query_row(
        "SELECT s.name, s.goal, s.starts_at, s.ends_at, s.closed_at, p.slug FROM sprints s LEFT JOIN projects p ON p.id = s.project_id WHERE s.id = ?1",
        [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
    let work = tasks(db, &format!("{TASK_SELECT} WHERE t.deleted_at IS NULL AND t.sprint_id = ?"), vec![Sql::Integer(id)])?;
    let done_at: Vec<(i64, Option<i64>)> = db.prepare("SELECT COALESCE(points, 1), done_at FROM tasks WHERE deleted_at IS NULL AND sprint_id = ?1")?
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let uses_points = work.iter().any(|t| t.points.is_some());
    let weight = |w: i64| if uses_points { w } else { 1 };
    let scope: i64 = done_at.iter().map(|(w, _)| weight(*w)).sum();
    let finished: i64 = done_at.iter().filter(|(_, d)| d.is_some()).map(|(w, _)| weight(*w)).sum();

    let mut days = vec![];
    let mut day = starts;
    let span = ((ends - starts) as f64 / DAY as f64).round().max(1.0);
    while day < ends && day <= now {
        let end = next_day(day);
        let left = scope - done_at.iter().filter(|(_, d)| d.is_some_and(|d| d < end)).map(|(w, _)| weight(*w)).sum::<i64>();
        let ideal = scope as f64 * (1.0 - ((end - starts) as f64 / DAY as f64).round() / span);
        days.push(json!({ "day": fmt_day(day), "remaining": left, "ideal": (ideal * 10.0).round() / 10.0 }));
        day = end;
    }
    let state = if closed.is_some() { "closed" } else if now < starts { "planned" } else if now >= ends { "over" } else { "running" };
    Ok(json!({
        "id": fmt_id(id),
        "name": name,
        "goal": goal,
        "project": project,
        "state": state,
        "starts": fmt_day(starts),
        "ends": fmt_day(ends - 1),
        "days_left": if state == "running" { ((ends - now) as f64 / DAY as f64).ceil() as i64 } else { 0 },
        "unit": if uses_points { "points" } else { "tasks" },
        "done": finished,
        "scope": scope,
        "tasks": { "done": work.iter().filter(|t| t.status == "done").count(), "total": work.len() },
        "burndown": days,
        "open": work.iter().filter(|t| t.status != "done").map(Task::brief).collect::<Value>(),
    }))
}

/// Open sprints (or every sprint with `all`), newest first, each with its progress.
pub fn list_sprints(db: &Connection, all: bool, now: i64) -> Result<Vec<Value>> {
    let ids: Vec<i64> = db.prepare(&format!("SELECT id FROM sprints {} ORDER BY starts_at DESC, id DESC", if all { "" } else { "WHERE closed_at IS NULL" }))?
        .query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    ids.into_iter().map(|id| {
        let mut s = sprint_detail(db, &id.to_string(), now)?;
        s.as_object_mut().map(|o| { o.shift_remove("burndown"); o.shift_remove("open"); });
        Ok(s)
    }).collect()
}

/// Closes a sprint; its unfinished tasks stay where they are and come back in the answer, to move on or put in the next sprint.
pub fn close_sprint(db: &Connection, r: &str, now: i64) -> Result<Value> {
    let id = sprint_ref(db, r, None, now)?.ok_or_else(|| Error("which sprint? give its id or \"current\"".into()))?;
    tx(db, || {
        db.execute("UPDATE sprints SET closed_at = ?2 WHERE id = ?1", params![id, now])?;
        log_event(db, "sprint.close", None, json!({ "sprint": id }), now)?;
        Ok(())
    })?;
    let mut s = sprint_detail(db, &id.to_string(), now)?;
    if let Some(o) = s.as_object_mut() {
        let open = o.shift_remove("open").unwrap_or_default();
        o.insert("unfinished".into(), open);
        o.shift_remove("burndown");
    }
    Ok(s)
}

fn running_sprints(db: &Connection, now: i64) -> Result<Vec<Value>> {
    Ok(list_sprints(db, false, now)?.into_iter().filter(|s| s["state"] == "running").collect())
}

// ---------- time ----------

/// Starts tracking time on a task (stopping any timer already running). A task in backlog or todo moves to doing.
pub fn start_timer(db: &Connection, r: &str, now: i64) -> Result<Value> {
    let id = parse_id(r)?;
    let t = get_row(db, id)?;
    if t.deleted_at.is_some() || t.status == "done" {
        return err(format!("{} is {}; time goes on open tasks", fmt_id(id), if t.status == "done" { "done" } else { "deleted" }));
    }
    let stopped = stop_timer(db, now).ok();
    db.execute("INSERT INTO time_entries (task_id, started_at) VALUES (?1, ?2)", params![id, now])?;
    let t = if t.status == "backlog" || t.status == "todo" { update_task(db, &id.to_string(), &Patch { status: Some("doing"), ..Default::default() }, now)? } else { t };
    Ok(json!({ "started": t.view(), "stopped": stopped }))
}

/// Stops the running timer; says what it was on and for how long.
pub fn stop_timer(db: &Connection, now: i64) -> Result<Value> {
    let Some((entry, task, started)) = running_timer(db)? else { return err("no timer is running") };
    db.execute("UPDATE time_entries SET ended_at = ?2 WHERE id = ?1", params![entry, now.max(started)])?;
    let t = get_row(db, task)?;
    Ok(json!({ "id": fmt_id(task), "title": t.title, "this_time": fmt_minutes(now - started), "time_spent": fmt_minutes(t.spent) }))
}

/// (entry, task, started_at) of the running timer.
pub fn running_timer(db: &Connection) -> Result<Option<(i64, i64, i64)>> {
    Ok(db.query_row("SELECT id, task_id, started_at FROM time_entries WHERE ended_at IS NULL ORDER BY started_at DESC LIMIT 1", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?)
}

/// Tracked time inside [from, to): per task (most first) and in total.
fn time_in(db: &Connection, from: i64, to: i64) -> Result<Value> {
    let rows: Vec<(i64, String, Option<String>, i64)> = db.prepare(
        "SELECT t.id, t.title, p.slug, SUM(MIN(COALESCE(e.ended_at, ?2), ?2) - MAX(e.started_at, ?1)) AS ms
         FROM time_entries e JOIN tasks t ON t.id = e.task_id LEFT JOIN projects p ON p.id = t.project_id
         WHERE e.started_at < ?2 AND COALESCE(e.ended_at, ?2) > ?1 GROUP BY t.id ORDER BY ms DESC")?
        .query_map([from, to], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
    let total: i64 = rows.iter().map(|r| r.3).sum();
    Ok(json!({
        "total": fmt_minutes(total),
        "minutes": total / 60_000,
        "tasks": rows.iter().take(8).map(|(id, title, project, ms)| json!({ "id": fmt_id(*id), "title": title, "project": project, "time": fmt_minutes(*ms) })).collect::<Value>(),
    }))
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

/// Workdays in a row, back from today, with at least one task done. A quiet weekend or a quiet today doesn't break it.
fn streak(per_day: &[u64], days: &[i64]) -> u64 {
    let mut n = 0;
    for (i, (&d, &day)) in per_day.iter().zip(days).enumerate().rev() {
        match d {
            0 if i == per_day.len() - 1 || crate::time::is_weekend(day) => {}
            0 => break,
            _ => n += 1,
        }
    }
    n
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    xs.sort_by(f64::total_cmp);
    let n = xs.len();
    (n > 0).then(|| if n % 2 == 1 { xs[n / 2] } else { (xs[n / 2 - 1] + xs[n / 2]) / 2.0 })
}

/// The numbers behind a daily or weekly report: throughput, streak, cycle time, time blocked, what's sitting still and what carries over.
pub fn report(db: &Connection, period: &str, now: i64) -> Result<Value> {
    let today = start_of_day(now);
    let from = match period {
        "today" => today,
        "week" => start_of_day(today - 6 * DAY),
        _ => return err("period must be today or week"),
    };
    let days = crate::time::last_days(14, now);
    let per_day = done_per_day(db, 14, now)?;
    let long = done_per_day(db, 60, now)?;
    let open = list_tasks(db, &Filter::default(), now)?;
    let hours = |ms: i64| ms as f64 / 3_600_000.0;
    let round = |h: f64| (h * 10.0).round() / 10.0;

    // cycle time: first move into doing (or creation) until done, for tasks finished in the window
    let cycles: Vec<f64> = db.prepare(
        "SELECT t.done_at - COALESCE((SELECT MIN(e.created_at) FROM events e WHERE e.task_id = t.id AND (
             (e.type = 'task.status' AND json_extract(e.payload, '$.to') = 'doing') OR
             (e.type = 'task.add' AND json_extract(e.payload, '$.status') = 'doing'))), t.created_at)
         FROM tasks t WHERE t.deleted_at IS NULL AND t.done_at >= ?1 AND t.done_at < ?2")?
        .query_map([from, now], |r| r.get::<_, i64>(0))?.map(|d| d.map(|d| hours(d.max(0)))).collect::<rusqlite::Result<_>>()?;

    // time blocked inside the window, from the status events
    let mut since = std::collections::HashMap::new();
    let (mut blocked_ms, mut blocked_tasks) = (0, std::collections::HashSet::new());
    let mut clip = |task: i64, a: i64, b: i64| {
        let ms = b.min(now) - a.max(from);
        if ms > 0 {
            blocked_ms += ms;
            blocked_tasks.insert(task);
        }
    };
    let mut q = db.prepare("SELECT e.task_id, e.created_at, e.type, e.payload FROM events e JOIN tasks t ON t.id = e.task_id
        WHERE t.deleted_at IS NULL AND e.type IN ('task.add', 'task.status') AND e.created_at < ?1 ORDER BY e.created_at, e.id")?;
    for row in q.query_map([now], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))? {
        let (task, at, kind, payload) = row?;
        let p: Value = serde_json::from_str(&payload).unwrap_or_default();
        let to = if kind == "task.add" { &p["status"] } else { &p["to"] };
        if p["from"] == "blocked" {
            if let Some(start) = since.remove(&task) {
                clip(task, start, at);
            }
        }
        if to == "blocked" {
            since.insert(task, at);
        }
    }
    for (task, start) in since {
        clip(task, start, now);
    }

    // tasks sitting in doing, review or blocked, by how long since their last move
    let mut aging: Vec<(i64, String, String, f64)> = db.prepare(
        "SELECT t.id, t.title, t.status, COALESCE((SELECT MAX(e.created_at) FROM events e WHERE e.task_id = t.id AND e.type = 'task.status'), t.created_at)
         FROM tasks t WHERE t.deleted_at IS NULL AND t.status IN ('doing', 'review', 'blocked')")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, (now - r.get::<_, i64>(3)?) as f64 / DAY as f64)))?
        .collect::<rusqlite::Result<_>>()?;
    aging.sort_by(|a, b| b.3.total_cmp(&a.3));

    let tomorrow = next_day(today);
    let done_in = |a: i64| -> Result<i64> { Ok(db.query_row("SELECT COUNT(*) FROM tasks WHERE deleted_at IS NULL AND done_at >= ?1 AND done_at < ?2", [a, now], |r| r.get(0))?) };
    Ok(json!({
        "period": period,
        "window": { "from": iso(from), "to": iso(now), "label": if period == "week" { format!("{} – {}", fmt_day(from), fmt_day(now)) } else { fmt_day(from) } },
        "totals": {
            "done": done_in(from)?,
            "added": db.query_row("SELECT COUNT(*) FROM tasks WHERE deleted_at IS NULL AND created_at >= ?1 AND created_at < ?2", [from, now], |r| r.get::<_, i64>(0))?,
            "open": open.len(),
            "doing": open.iter().filter(|t| t.status == "doing").count(),
            "blocked": open.iter().filter(|t| t.status == "blocked").count(),
            "overdue": open.iter().filter(|t| t.due_at.is_some_and(|d| d < today)).count(),
        },
        "streak_days": streak(&long, &crate::time::last_days(60, now)),
        "done_per_day": per_day.iter().zip(&days).map(|(n, d)| json!({ "day": fmt_day(*d), "done": n })).collect::<Value>(),
        "cycle_time_hours": { "tasks": cycles.len(), "median": median(cycles.clone()).map(round), "average": (!cycles.is_empty()).then(|| round(cycles.iter().sum::<f64>() / cycles.len() as f64)) },
        "blocked_hours": { "total": round(hours(blocked_ms)), "tasks": blocked_tasks.len() },
        "projects": project_stats(db, from)?.into_iter().map(|p| json!({ "project": p.slug, "open": p.open, "done": p.done })).collect::<Value>(),
        "sitting_still": aging.iter().take(8).map(|(id, title, status, days)| json!({ "id": fmt_id(*id), "title": title, "status": status, "days": round(*days) })).collect::<Value>(),
        "carry_over": open.iter().filter(|t| matches!(t.status.as_str(), "doing" | "review") || t.due_at.is_some_and(|d| d < tomorrow)).map(Task::brief).collect::<Value>(),
        "time": time_in(db, from, now)?,
        "sprints": running_sprints(db, now)?,
    }))
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
        "time": time_in(db, from, to)?,
        "sprints": running_sprints(db, now)?,
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
    fn migration_2_upgrades_an_existing_board_in_place() {
        let file = std::env::temp_dir().join(format!("huddle-upgrade-{}.db", std::process::id()));
        let _ = fs::remove_file(&file);
        {
            let old = Connection::open(&file).unwrap();
            old.execute_batch(&format!("{}\nPRAGMA user_version = 1;", MIGRATIONS[0])).unwrap();
            old.execute("INSERT INTO tasks (title, created_at, updated_at) VALUES ('From before', 1, 1)", []).unwrap();
        }
        let db = open_db(&file.to_string_lossy()).unwrap();
        assert_eq!(db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        let t = update_task(&db, "1", &Patch { points: Some("3"), tags: Some("old, Kept"), ..Default::default() }, 2).unwrap();
        assert_eq!((t.title.as_str(), t.points, t.tags.clone()), ("From before", Some(3), vec!["kept".to_string(), "old".to_string()]));
        drop(db);
        for ext in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(format!("{}{ext}", file.to_string_lossy()));
        }
    }

    #[test]
    fn tags_points_and_subtasks() {
        let db = db();
        let parent = add_task(&db, &NewTask { title: "Checkout v2", points: Some("8"), tags: Some("frontend, #Payments, frontend"), ..Default::default() }, 1).unwrap();
        assert_eq!(parent.tags, ["frontend", "payments"]);
        add_task(&db, &NewTask { title: "Card form", parent: Some("#1"), ..Default::default() }, 1).unwrap();
        add_task(&db, &NewTask { title: "Receipt email", parent: Some("1"), status: Some("done"), ..Default::default() }, 1).unwrap();
        let p = get_task(&db, 1).unwrap();
        assert_eq!(p.subtasks, (1, 2));
        assert_eq!(p.view()["subtasks"], "1/2 done");
        let only = |f: Filter| list_tasks(&db, &f, 2).unwrap().into_iter().map(|t| t.title).collect::<Vec<_>>();
        assert_eq!(only(Filter { parent: Some("1"), status: Some("all"), ..Default::default() }), ["Card form", "Receipt email"]);
        assert_eq!(only(Filter { tag: Some("payments"), ..Default::default() }), ["Checkout v2"]);
        assert!(update_task(&db, "1", &Patch { parent: Some("1"), ..Default::default() }, 2).is_err());
        assert!(add_task(&db, &NewTask { title: "x", points: Some("lots"), ..Default::default() }, 1).is_err());
        let cleared = update_task(&db, "1", &Patch { points: Some("none"), tags: Some(""), ..Default::default() }, 3).unwrap();
        assert_eq!((cleared.points, cleared.tags.len()), (None, 0));
    }

    #[test]
    fn finishing_a_repeating_task_makes_the_next_one() {
        let db = db();
        let fri = at("2026-10-30T10:00");
        let t = add_task(&db, &NewTask { title: "Weekly sync notes", recur: Some("weekly"), due: Some("2026-10-30T09:00"), tags: Some("ritual"), ..Default::default() }, fri).unwrap();
        let done = update_task(&db, &t.id.to_string(), &Patch { status: Some("done"), ..Default::default() }, fri).unwrap();
        let next = get_task(&db, done.next.unwrap()).unwrap();
        assert_eq!((next.status.as_str(), next.due_at, next.recur.as_deref(), next.tags.clone()), ("todo", Some(at("2026-11-06T09:00")), Some("weekly"), vec!["ritual".to_string()]));
        assert_eq!(next_due("weekdays", at("2026-10-30T18:00"), fri), at("2026-11-02T18:00")); // Friday → Monday
        assert_eq!(next_due("monthly", at("2026-10-31T18:00"), fri), at("2026-11-30T18:00"));
        assert_eq!(next_due("daily", at("2026-10-20T18:00"), fri), at("2026-10-30T18:00")); // done late: next lands after now
        assert!(add_task(&db, &NewTask { title: "x", recur: Some("hourly"), ..Default::default() }, fri).is_err());
    }

    #[test]
    fn sprints_track_progress_and_burn_down() {
        let db = db();
        let h = 3_600_000;
        let mon = at("2026-10-26T09:00");
        let s = add_sprint(&db, "Sprint 1", None, Some("2026-10-26"), Some("2026-10-30"), Some("Ship checkout"), mon).unwrap();
        assert_eq!((s["id"].as_str(), s["state"].as_str(), s["ends"].as_str()), (Some("#1"), Some("running"), Some("Fri 30 Oct")));
        for (title, pts) in [("A", "3"), ("B", "5"), ("C", "2")] {
            add_task(&db, &NewTask { title, points: Some(pts), sprint: Some("current"), ..Default::default() }, mon).unwrap();
        }
        update_task(&db, "1", &Patch { status: Some("done"), ..Default::default() }, mon + 5 * h).unwrap();
        update_task(&db, "3", &Patch { status: Some("done"), ..Default::default() }, mon + 30 * h).unwrap();
        let d = sprint_detail(&db, "current", mon + 50 * h).unwrap(); // Wed
        assert_eq!((d["done"].as_i64(), d["scope"].as_i64(), d["unit"].as_str()), (Some(5), Some(10), Some("points")));
        let left: Vec<i64> = d["burndown"].as_array().unwrap().iter().map(|x| x["remaining"].as_i64().unwrap()).collect();
        assert_eq!(left, [7, 5, 5]); // Mon, Tue, Wed
        assert_eq!(d["days_left"], 3);
        assert_eq!(list_tasks(&db, &Filter { sprint: Some("1"), ..Default::default() }, mon).unwrap().len(), 1);
        let closed = close_sprint(&db, "1", mon + 60 * h).unwrap();
        assert_eq!((closed["state"].as_str(), closed["unfinished"].as_array().unwrap().len()), (Some("closed"), 1));
        assert!(list_sprints(&db, false, mon).unwrap().is_empty());
        assert_eq!(list_sprints(&db, true, mon).unwrap().len(), 1);
        assert!(sprint_ref(&db, "current", None, mon + 60 * h).is_err());
        assert!(add_sprint(&db, "Backwards", None, Some("2026-10-30"), Some("2026-10-20"), None, mon).is_err());
    }

    #[test]
    fn timers_track_time_and_one_runs_at_a_time() {
        let db = db();
        let h = 3_600_000;
        let t0 = at("2026-10-28T09:00");
        add(&db, "Write docs", None, None, t0);
        add(&db, "Fix bug", None, None, t0);
        let started = start_timer(&db, "1", t0).unwrap();
        assert_eq!(started["started"]["status"], "doing");
        let switched = start_timer(&db, "2", t0 + 2 * h).unwrap(); // stops the first
        assert_eq!(switched["stopped"]["this_time"], "2h00");
        let stopped = stop_timer(&db, t0 + 2 * h + 30 * 60_000).unwrap();
        assert_eq!((stopped["id"].as_str(), stopped["this_time"].as_str()), (Some("#2"), Some("30m")));
        assert!(stop_timer(&db, t0 + 3 * h).is_err());
        assert_eq!(get_task(&db, 1).unwrap().spent, 2 * h);
        let r = report(&db, "today", t0 + 4 * h).unwrap();
        assert_eq!((r["time"]["total"].as_str(), r["time"]["tasks"][0]["title"].as_str()), (Some("2h30"), Some("Write docs")));
        update_task(&db, "1", &Patch { status: Some("done"), ..Default::default() }, t0 + 5 * h).unwrap();
        assert!(start_timer(&db, "1", t0 + 6 * h).is_err());
    }

    #[test]
    fn report_measures_cycle_time_blocked_time_streak_and_what_sits_still() {
        let db = db();
        let h = 3_600_000;
        let mon = at("2026-10-26T10:00");
        let (tue, wed) = (mon + 24 * h, mon + 48 * h);
        let to = |id: &str, status, reason, at| update_task(&db, id, &Patch { status: Some(status), blocked_reason: reason, ..Default::default() }, at).unwrap();
        add(&db, "A", None, None, mon);
        to("1", "doing", None, tue);
        to("1", "done", None, wed);
        add(&db, "B", Some("doing"), None, mon);
        to("2", "done", None, mon + 6 * h);
        add(&db, "C", None, None, mon);
        to("3", "blocked", Some("keys"), tue + 2 * h);
        to("3", "doing", None, tue + 4 * h);
        add(&db, "D", None, None, mon);
        to("4", "blocked", Some("design"), wed - h);

        let r = report(&db, "week", wed + 2 * h).unwrap();
        assert_eq!(r["totals"]["done"], 2);
        assert_eq!(r["cycle_time_hours"]["median"], 15.0); // 6 h and 24 h
        assert_eq!(r["blocked_hours"], json!({ "total": 5.0, "tasks": 2 })); // C for 2 h, D for 3 h so far
        assert_eq!(r["streak_days"], 1); // done Wed, nothing Tue
        assert_eq!(r["sitting_still"][0]["title"], "C");
        assert_eq!(r["carry_over"].as_array().unwrap().len(), 1); // C, in doing
        assert!(report(&db, "year", wed).is_err());
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
