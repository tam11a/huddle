// The interactive board: state, keys and the event loop. Drawing lives in ui.rs.
// Same file and rules as the commands, so stand-ups see every move made here.
use crate::board::{self, Filter, NewTask, Patch, ProjectStat, Result, STATUSES, Task, fmt_id};
use crate::time::{DAY, now, start_of_day};
use crate::ui;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rusqlite::Connection;
use serde_json::Value;
use std::time::{Duration, Instant};

const FLOW: [&str; 5] = ["backlog", "todo", "doing", "review", "done"]; // < and > step along this; blocked sits beside doing
pub const PRIORITY: [&str; 4] = ["urgent", "high", "normal", "low"];
const REFRESH: Duration = Duration::from_secs(2); // picks up what Claude changes while the board is open

pub struct Board {
    /// One list of tasks per status, in STATUSES order; done holds only what was done today.
    pub cols: Vec<Vec<Task>>,
    pub per_day: Vec<u64>,
    pub projects: Vec<ProjectStat>,
}

pub fn load_board(db: &Connection, now: i64) -> Result<Board> {
    // shortcut: shows at most 1000 tasks, page the columns if a board ever grows past that
    let f = Filter { status: Some("all"), done_since: Some(start_of_day(now)), limit: Some(1000), ..Default::default() };
    let tasks = board::list_tasks(db, &f, now)?;
    Ok(Board {
        cols: STATUSES.iter().map(|s| tasks.iter().filter(|t| t.status == *s).cloned().collect()).collect(),
        per_day: board::done_per_day(db, 14, now)?,
        projects: board::project_stats(db, start_of_day(start_of_day(now) - 6 * DAY))?,
    })
}

pub enum Action {
    Add,
    Search,
    Block(i64),
    Title(i64),
    Description(i64),
    Project(i64),
    Due(i64),
    Delete(i64),
}

/// One step back: the task as it was before the last change, or the task the last add created.
enum Undo {
    Changed(Task),
    Added(i64),
}

pub struct Focus {
    pub id: i64,
    pub title: String,
    pub started: Instant,
    pub length: Duration,
}

const FOCUS: Duration = Duration::from_secs(25 * 60);

pub struct Prompt {
    pub label: String,
    pub value: String,
    pub confirm: bool,
    pub opened: Instant,
    pub action: Action,
}

pub struct State {
    pub col: usize,
    pub rows: Vec<i64>,
    pub prompt: Option<Prompt>,
    pub standup: Option<(Value, Value)>,
    pub report: Option<(Value, Instant)>,
    pub help: bool,
    pub msg: String,
    pub err: bool,
    pub quit: bool,
    // what's shown: a text filter, a project filter, and the list view instead of columns
    pub filter: String,
    pub project: Option<String>,
    pub list: bool,
    pub list_row: i64,
    pub focus: Option<Focus>,
    undo: Option<Undo>,
    // motion: when things happened, and the eased values the drawing chases
    pub started: Instant,
    pub intro: Option<Instant>,
    pub toast: Instant,
    pub flash: Option<(i64, Instant)>,
    pub gauge: f64,
    pub gauge_to: f64,
    pub grow: f64,
}

impl State {
    pub fn new() -> Self {
        let t = Instant::now();
        State {
            col: 2,
            rows: vec![0; STATUSES.len()],
            prompt: None,
            standup: None,
            report: None,
            help: false,
            msg: String::new(),
            err: false,
            quit: false,
            filter: String::new(),
            project: None,
            list: false,
            list_row: 0,
            focus: None,
            undo: None,
            started: t,
            intro: None,
            toast: t,
            flash: None,
            gauge: 0.0,
            gauge_to: 0.0,
            grow: 0.0,
        }
    }
}

/// Clamps the cursor to the board and returns the selected task, if any.
pub fn pick<'b>(s: &mut State, cols: &'b [Vec<Task>]) -> Option<&'b Task> {
    for (r, col) in s.rows.iter_mut().zip(cols) {
        *r = (*r).min(col.len() as i64 - 1).max(0);
    }
    cols[s.col].get(s.rows[s.col] as usize)
}

/// The board's columns after the text and project filters; while searching, the text being typed filters live.
pub fn shown(b: &Board, s: &State) -> Vec<Vec<Task>> {
    let text = match &s.prompt {
        Some(p) if matches!(p.action, Action::Search) => p.value.to_lowercase(),
        _ => s.filter.to_lowercase(),
    };
    let keep = |t: &Task| {
        s.project.as_ref().is_none_or(|p| t.project.as_ref() == Some(p))
            && (text.is_empty() || fmt_id(t.id) == text || t.title.to_lowercase().contains(&text) || t.description.to_lowercase().contains(&text))
    };
    b.cols.iter().map(|c| c.iter().filter(|t| keep(t)).cloned().collect()).collect()
}

/// Every shown task in one list, most urgent first: doing, blocked, review, todo, backlog, done.
pub fn listed(cols: &[Vec<Task>]) -> Vec<Task> {
    [2, 4, 3, 1, 0, 5].iter().flat_map(|&i| cols[i].iter().cloned()).collect()
}

/// The selected task, in whichever view is showing.
pub fn selected(s: &mut State, cols: &[Vec<Task>]) -> Option<Task> {
    if s.list {
        let l = listed(cols);
        s.list_row = s.list_row.min(l.len() as i64 - 1).max(0);
        l.get(s.list_row as usize).cloned()
    } else {
        pick(s, cols).cloned()
    }
}

fn follow(s: &mut State, db: &Connection, id: i64, now: i64) -> Result<()> {
    let cols = shown(&load_board(db, now)?, s);
    for (c, col) in cols.iter().enumerate() {
        if let Some(r) = col.iter().position(|t| t.id == id) {
            s.col = c;
            s.rows[c] = r as i64;
        }
    }
    if let Some(r) = listed(&cols).iter().position(|t| t.id == id) {
        s.list_row = r as i64;
    }
    Ok(())
}

/// Ends the focus session when its time is up.
pub fn tick(s: &mut State, t: Instant) {
    if let Some(f) = s.focus.take_if(|f| t.saturating_duration_since(f.started) >= f.length) {
        s.msg = format!("focus done: {} min on {} {}", f.length.as_secs() / 60, fmt_id(f.id), f.title);
        s.err = false;
        s.toast = t;
        s.flash = Some((f.id, t));
    }
}

fn undo(s: &mut State, db: &Connection, now: i64) -> Result<()> {
    match s.undo.take() {
        None => s.msg = "nothing to undo".into(),
        Some(Undo::Added(id)) => {
            let t = board::update_task(db, &id.to_string(), &Patch { delete: true, ..Default::default() }, now)?;
            s.msg = format!("{} {}: add undone", fmt_id(t.id), t.title);
        }
        Some(Undo::Changed(p)) => {
            let cur = board::get_task(db, p.id)?;
            let (priority, project) = (p.priority.to_string(), p.project.clone().unwrap_or_default());
            let due = p.due_at.map_or("none".into(), crate::time::fmt_local_iso);
            let t = board::update_task(db, &p.id.to_string(), &Patch {
                status: Some(&p.status), blocked_reason: p.blocked_reason.as_deref(), title: Some(&p.title), description: Some(&p.description),
                priority: Some(&priority), due: Some(&due), project: Some(&project), restore: cur.deleted_at.is_some() && p.deleted_at.is_none(),
                delete: false,
            }, now)?;
            follow(s, db, t.id, now)?;
            s.msg = format!("{} {}: undone", fmt_id(t.id), t.title);
            s.flash = Some((t.id, Instant::now()));
        }
    }
    Ok(())
}

fn ask(s: &mut State, label: &str, value: &str, action: Action) {
    s.prompt = Some(Prompt { label: label.into(), value: value.into(), confirm: false, opened: Instant::now(), action });
}

fn change(s: &mut State, db: &Connection, id: i64, p: &Patch, what: &str, go: bool, now: i64) -> Result<()> {
    let before = board::get_task(db, id)?;
    let t = board::update_task(db, &id.to_string(), p, now)?;
    s.undo = Some(Undo::Changed(before));
    if go {
        follow(s, db, t.id, now)?;
    }
    s.msg = format!("{} {}: {what}", fmt_id(t.id), t.title);
    s.flash = Some((t.id, Instant::now()));
    Ok(())
}

/// Applies one key ("left", "return", "a", "\x03", …) to the state and the board. Errors become the toast.
pub fn press(s: &mut State, k: &str, db: &Connection, now: i64) {
    if k == "\x03" {
        s.quit = true;
        return;
    }
    let before = s.msg.clone();
    s.err = false;
    let r = if s.prompt.is_some() {
        type_into(s, k, db, now)
    } else if s.help || s.standup.is_some() || s.report.is_some() {
        s.help = false;
        s.standup = None;
        s.report = None;
        Ok(())
    } else {
        board_key(s, k, db, now)
    };
    if let Err(e) = r {
        s.msg = e.0;
        s.err = true;
        s.prompt = None;
    }
    if s.err || s.msg != before {
        s.toast = Instant::now();
    }
}

fn type_into(s: &mut State, k: &str, db: &Connection, now: i64) -> Result<()> {
    let Some(p) = s.prompt.as_mut() else { return Ok(()) };
    match k {
        _ if p.confirm => {
            let p = s.prompt.take().unwrap();
            if k == "y" {
                submit(s, p.action, "", db, now)?;
            }
        }
        "escape" => {
            if matches!(p.action, Action::Search) {
                s.filter.clear();
            }
            s.prompt = None;
        }
        "return" => {
            let p = s.prompt.take().unwrap();
            submit(s, p.action, p.value.trim(), db, now)?;
        }
        "backspace" => {
            p.value.pop();
        }
        _ if k.chars().count() == 1 && k >= " " => p.value.push_str(k),
        _ => {}
    }
    Ok(())
}

fn submit(s: &mut State, a: Action, v: &str, db: &Connection, now: i64) -> Result<()> {
    match a {
        Action::Add => {
            let here = STATUSES[s.col];
            let status = if here == "blocked" || here == "done" { "todo" } else { here };
            let t = board::add_task(db, &NewTask { title: v, status: Some(status), ..Default::default() }, now)?;
            follow(s, db, t.id, now)?;
            s.msg = format!("{} {}: added", fmt_id(t.id), t.title);
            s.flash = Some((t.id, Instant::now()));
            s.undo = Some(Undo::Added(t.id));
            Ok(())
        }
        Action::Search => {
            s.filter = v.to_string();
            Ok(())
        }
        Action::Description(id) => change(s, db, id, &Patch { description: Some(v), ..Default::default() }, "description saved", false, now),
        Action::Project(id) => change(s, db, id, &Patch { project: Some(v), ..Default::default() }, if v.is_empty() { "project cleared" } else { "project set" }, false, now),
        Action::Block(id) => change(s, db, id, &Patch { status: Some("blocked"), blocked_reason: Some(v), ..Default::default() }, "→ blocked", false, now),
        Action::Title(id) => change(s, db, id, &Patch { title: Some(v), ..Default::default() }, "renamed", false, now),
        Action::Due(_) if v.is_empty() => Ok(()),
        Action::Due(id) => change(s, db, id, &Patch { due: Some(v), ..Default::default() }, "due date set", false, now),
        Action::Delete(id) => change(s, db, id, &Patch { delete: true, ..Default::default() }, "deleted", false, now),
    }
}

fn board_key(s: &mut State, k: &str, db: &Connection, now: i64) -> Result<()> {
    let b = load_board(db, now)?;
    let cols = shown(&b, s);
    let t = selected(s, &cols);
    match k {
        "left" | "h" if !s.list => s.col = s.col.saturating_sub(1),
        "right" | "l" if !s.list => s.col = (s.col + 1).min(STATUSES.len() - 1),
        "up" | "k" if s.list => s.list_row -= 1,
        "down" | "j" if s.list => s.list_row += 1,
        "up" | "k" => s.rows[s.col] -= 1,
        "down" | "j" => s.rows[s.col] += 1,
        "tab" => s.list = !s.list,
        "q" => s.quit = true,
        "?" => s.help = true,
        "s" => s.standup = Some((board::facts(db, "last_workday", now)?, board::facts(db, "today", now)?)),
        "r" => s.report = Some((board::report(db, "week", now)?, Instant::now())),
        "a" => ask(s, "New task", "", Action::Add),
        "u" => undo(s, db, now)?,
        "/" => {
            let current = s.filter.clone();
            ask(s, "Search", &current, Action::Search);
        }
        "P" => {
            let slugs: Vec<&String> = b.projects.iter().map(|p| &p.slug).collect();
            let next = match &s.project {
                None => slugs.first().copied(),
                Some(p) => slugs.iter().skip_while(|x| **x != p).nth(1).copied(),
            };
            s.project = next.cloned();
            s.msg = match &s.project { Some(p) => format!("showing project {p}"), None => "showing all projects".into() };
        }
        "escape" if !s.filter.is_empty() || s.project.is_some() => {
            s.filter.clear();
            s.project = None;
            s.msg = "filters cleared".into();
        }
        "f" if s.focus.is_some() => {
            let f = s.focus.take().unwrap();
            s.msg = format!("focus stopped after {} min on {}", f.started.elapsed().as_secs() / 60, fmt_id(f.id));
        }
        _ => {
            let Some(t) = t else { return Ok(()) }; // the rest act on the selected task
            let i = FLOW.iter().position(|f| *f == if t.status == "blocked" { "doing" } else { &t.status }).unwrap_or(0);
            let to = |st| Patch { status: Some(st), ..Default::default() };
            match k {
                "<" if i > 0 => change(s, db, t.id, &to(FLOW[i - 1]), &format!("→ {}", FLOW[i - 1]), true, now)?,
                ">" if i + 1 < FLOW.len() => change(s, db, t.id, &to(FLOW[i + 1]), &format!("→ {}", FLOW[i + 1]), true, now)?,
                "d" => change(s, db, t.id, &to("done"), "→ done", false, now)?,
                "b" if t.status == "blocked" => change(s, db, t.id, &to("doing"), "unblocked", false, now)?,
                "b" => ask(s, "Blocked on", "", Action::Block(t.id)),
                "e" => ask(s, "Title", &t.title, Action::Title(t.id)),
                "E" => ask(s, "Description", &t.description, Action::Description(t.id)),
                "o" => {
                    let known: Vec<&str> = b.projects.iter().map(|p| p.slug.as_str()).collect();
                    let label = format!("Project ({}; empty clears)", if known.is_empty() { "none registered".into() } else { known.join(", ") });
                    ask(s, &label, t.project.as_deref().unwrap_or(""), Action::Project(t.id));
                }
                "f" => {
                    if t.status == "backlog" || t.status == "todo" {
                        change(s, db, t.id, &to("doing"), "→ doing", false, now)?;
                    }
                    s.focus = Some(Focus { id: t.id, title: t.title.clone(), started: Instant::now(), length: FOCUS });
                    s.msg = format!("focus: {} min on {} {}", FOCUS.as_secs() / 60, fmt_id(t.id), t.title);
                }
                "t" => ask(s, "Due (fri, tomorrow 9:30, none)", "", Action::Due(t.id)),
                "p" => {
                    let p = (t.priority + 3) % 4;
                    change(s, db, t.id, &Patch { priority: Some(&p.to_string()), ..Default::default() }, &format!("priority {}", PRIORITY[p as usize]), false, now)?
                }
                "x" => {
                    s.prompt = Some(Prompt { label: format!("Delete {} {}?", fmt_id(t.id), t.title), value: String::new(), confirm: true, opened: Instant::now(), action: Action::Delete(t.id) })
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn key_name(e: &KeyEvent) -> String {
    match e.code {
        KeyCode::Char('c') if e.modifiers.contains(KeyModifiers::CONTROL) => "\x03".into(),
        KeyCode::Char(c) => c.into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Enter => "return".into(),
        KeyCode::Esc => "escape".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Tab => "tab".into(),
        _ => String::new(),
    }
}

pub fn main() -> Result<()> {
    let file = board::db_file();
    let db = board::open_db(&file)?;
    let mut s = State::new();
    s.intro = Some(Instant::now());
    let mut term = ratatui::try_init()?; // raw mode + alternate screen, restored on panic too
    let out = run(&mut term, &db, &mut s, &file);
    ratatui::restore();
    out
}

fn run(term: &mut ratatui::DefaultTerminal, db: &Connection, s: &mut State, file: &str) -> Result<()> {
    let mut b = load_board(db, now())?;
    let mut loaded = Instant::now();
    while !s.quit {
        if loaded.elapsed() >= REFRESH {
            b = load_board(db, now())?;
            loaded = Instant::now();
        }
        let t = Instant::now();
        tick(s, t);
        term.draw(|f| ui::draw(f, s, &b, &ui::View { now: now(), t, file }))?;
        // ~30 fps while something moves, otherwise sleep until the next refresh or key
        if !event::poll(ui::next_frame(s, t).min(REFRESH.saturating_sub(loaded.elapsed())))? {
            continue;
        }
        if let Event::Key(e) = event::read()? {
            if e.kind == KeyEventKind::Release {
                continue;
            }
            if s.intro.take().is_none() {
                press(s, &key_name(&e), db, now()); // any key skips the intro instead
            }
            b = load_board(db, now())?;
            loaded = Instant::now();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::at;
    use ratatui::{Terminal, backend::TestBackend};

    fn keys(s: &mut State, db: &Connection, ks: &[&str]) {
        for k in ks {
            press(s, k, db, now());
        }
    }
    fn typed(text: &str) -> Vec<String> {
        text.chars().map(String::from).collect()
    }
    fn screen(s: &mut State, b: &Board, now: i64) -> Vec<String> {
        let (w, h) = (120, 32);
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| ui::draw(f, s, b, &ui::View { now, t: Instant::now(), file: "" })).unwrap();
        let buf = term.backend().buffer();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect()).collect()
    }
    fn has(lines: &[String], text: &str) -> bool {
        lines.iter().any(|l| l.contains(text))
    }
    fn task(db: &Connection) -> Task {
        board::list_tasks(db, &Filter { status: Some("all"), ..Default::default() }, now()).unwrap().remove(0)
    }

    #[test]
    fn board_has_a_column_per_status_and_done_shows_only_today() {
        let db = board::open_db(":memory:").unwrap();
        let mon = at("2026-10-26T10:00");
        let wed = at("2026-10-28T10:00");
        let add = |title, status, due, at| board::add_task(&db, &NewTask { title, status, due, ..Default::default() }, at).unwrap();
        add("Old win", Some("done"), None, mon);
        add("Fresh win", None, None, mon);
        board::update_task(&db, "#2", &Patch { status: Some("done"), ..Default::default() }, wed).unwrap();
        add("Late thing", None, Some("2026-10-27"), mon);

        let b = load_board(&db, wed).unwrap();
        assert_eq!(b.cols[5].iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Fresh win"]);
        assert_eq!(b.per_day[13], 1);
        let lines = screen(&mut State::new(), &b, wed);
        assert!(has(&lines, "1 open") && has(&lines, "1 overdue") && has(&lines, "1 done today"), "{}", lines.join("\n"));
        for st in STATUSES {
            assert!(has(&lines, st), "column {st}");
        }
        assert!(has(&lines, "#3 Late thing"));
        assert!(!has(&lines, "Old win"));
    }

    #[test]
    fn search_project_filter_list_view_undo_and_focus() {
        let db = board::open_db(":memory:").unwrap();
        board::add_project(&db, "web", "/tmp/web").unwrap();
        board::add_project(&db, "api", "/tmp/api").unwrap();
        let add = |title, status, project| board::add_task(&db, &NewTask { title, status: Some(status), project, ..Default::default() }, now()).unwrap();
        add("Write docs", "todo", Some("web"));
        add("Fix login", "doing", Some("api"));
        add("Deploy", "doing", None);
        let mut s = State::new();
        let count = |s: &State| shown(&load_board(&db, now()).unwrap(), s).iter().flatten().count();
        let status = |id| board::get_task(&db, id).unwrap();

        keys(&mut s, &db, &["/"]);
        keys(&mut s, &db, &typed("login").iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(count(&s), 1); // filters while typing
        keys(&mut s, &db, &["return"]);
        assert_eq!((s.filter.as_str(), count(&s)), ("login", 1));
        keys(&mut s, &db, &["escape"]);
        assert_eq!(count(&s), 3);

        keys(&mut s, &db, &["P"]);
        assert_eq!((s.project.as_deref(), count(&s)), (Some("api"), 1));
        keys(&mut s, &db, &["P", "P"]);
        assert_eq!(s.project, None);

        keys(&mut s, &db, &["tab", "down", "d"]); // list: Fix login, Deploy, Write docs
        assert_eq!(status(3).status, "done");
        keys(&mut s, &db, &["u"]);
        assert_eq!(status(3).status, "doing");
        keys(&mut s, &db, &["x", "y"]);
        assert!(status(3).deleted_at.is_some());
        keys(&mut s, &db, &["u"]);
        assert!(status(3).deleted_at.is_none());
        keys(&mut s, &db, &["tab", "a", "T", "return", "u"]);
        assert!(status(4).deleted_at.is_some()); // undoing an add deletes it again
        assert_eq!(s.msg, "#4 T: add undone");

        keys(&mut s, &db, &["left", "f"]); // focus on Write docs, in todo: it moves to doing
        assert_eq!(status(1).status, "doing");
        assert!(has(&screen(&mut s, &load_board(&db, now()).unwrap(), now()), "focus #1"));
        s.focus.as_mut().unwrap().started -= Duration::from_secs(26 * 60);
        tick(&mut s, Instant::now());
        assert!(s.focus.is_none() && s.msg.starts_with("focus done: 25 min on #1"));
    }

    #[test]
    fn intro_greets_then_gets_out_of_the_way() {
        let db = board::open_db(":memory:").unwrap();
        let b = load_board(&db, now()).unwrap();
        let mut s = State::new();
        s.intro = Some(Instant::now());
        assert!(has(&screen(&mut s, &b, now()), "huddle"));
        assert!(s.intro.is_some());
        s.intro = Some(Instant::now() - Duration::from_secs(5));
        assert!(has(&screen(&mut s, &b, now()), "backlog"));
        assert!(s.intro.is_none());
    }

    #[test]
    fn keys_add_move_block_unblock_edit_delete_and_overlays() {
        let db = board::open_db(":memory:").unwrap();
        let mut s = State::new(); // starts on doing
        let run = |s: &mut State, ks: Vec<String>| keys(s, &db, &ks.iter().map(String::as_str).collect::<Vec<_>>());

        run(&mut s, [vec!["a".into()], typed("Write docs"), vec!["return".into()]].concat());
        assert_eq!(task(&db).status, "doing");
        assert!(s.flash.is_some_and(|(id, _)| id == 1));
        keys(&mut s, &db, &[">"]);
        assert_eq!(task(&db).status, "review");
        assert_eq!(STATUSES[s.col], "review"); // the cursor follows the task

        keys(&mut s, &db, &["b", "return"]);
        assert!(s.err && s.msg.contains("reason"));
        run(&mut s, [vec!["b".into()], typed("no keys"), vec!["return".into()]].concat());
        assert_eq!(task(&db).blocked_reason.as_deref(), Some("no keys"));
        keys(&mut s, &db, &["right", "b"]);
        assert_eq!(task(&db).status, "doing");

        run(&mut s, [vec!["left".into(), "left".into(), "e".into()], vec!["backspace".into(); 4], typed("tests"), vec!["return".into(), "p".into()]].concat());
        assert_eq!(task(&db).title, "Write tests");
        assert_eq!(task(&db).priority, 1);
        assert!(has(&screen(&mut s, &load_board(&db, now()).unwrap(), now()), "doing 1"));

        keys(&mut s, &db, &["x"]);
        assert!(has(&screen(&mut s, &load_board(&db, now()).unwrap(), now()), "Delete #1 Write tests?"));
        keys(&mut s, &db, &["n"]);
        assert_eq!(board::list_tasks(&db, &Filter::default(), now()).unwrap().len(), 1);
        keys(&mut s, &db, &["x", "y"]);
        assert_eq!(board::list_tasks(&db, &Filter::default(), now()).unwrap().len(), 0);

        let b = load_board(&db, now()).unwrap();
        keys(&mut s, &db, &["?"]);
        assert!(has(&screen(&mut s, &b, now()), "raise the priority"));
        keys(&mut s, &db, &["s"]); // any key closes help, and does nothing else
        assert!(!s.help && s.standup.is_none());
        keys(&mut s, &db, &["s"]);
        assert!(has(&screen(&mut s, &b, now()), "Blockers"));
        keys(&mut s, &db, &["q", "r"]);
        let lines = screen(&mut s, &b, now());
        assert!(has(&lines, "cycle time") && has(&lines, "sitting still") && has(&lines, "carries over"));
        keys(&mut s, &db, &["q"]);
        assert!(s.standup.is_none() && s.report.is_none() && !s.quit);
        keys(&mut s, &db, &["q"]);
        assert!(s.quit);
    }
}
