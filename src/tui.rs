// The interactive board: same file and rules as the commands, so stand-ups see every move made here.
use crate::board::{self, Filter, NewTask, Patch, Result, STATUSES, Task, fmt_id};
use crate::time::{fmt_when, next_day, now, start_of_day};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use rusqlite::Connection;
use serde_json::Value;
use std::io::{self, Write};
use std::time::Duration;

const FLOW: [&str; 5] = ["backlog", "todo", "doing", "review", "done"]; // < and > step along this; blocked sits beside doing
const PRIORITY: [&str; 4] = ["urgent", "high", "normal", "low"];
const KEYS: &str = "←→↑↓ pick  < > move  d done  b block  a add  e title  t due  p priority  x delete  s today  q quit";

fn paint(code: &str, s: &str) -> String {
    if code.is_empty() { s.into() } else { format!("\x1b[{code}m{s}\x1b[0m") }
}

// shortcut: counts chars, so wide CJK/emoji titles can push a row past the edge; use a wcwidth table if that bites
fn fit(s: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    let mut c: Vec<char> = vec![];
    for ch in s.chars() {
        match ch {
            '\r' | '\n' | '\t' if c.last() == Some(&' ') => {}
            '\r' | '\n' | '\t' => c.push(' '),
            _ => c.push(ch),
        }
    }
    if c.len() > w {
        c.truncate(w - 1);
        c.push('…');
    } else {
        c.resize(w, ' ');
    }
    c.into_iter().collect()
}

/// One list of tasks per status, in STATUSES order; done holds only what was done today.
pub fn load_board(db: &Connection, now: i64) -> Result<Vec<Vec<Task>>> {
    // shortcut: shows at most 1000 tasks, page the columns if a board ever grows past that
    let f = Filter { status: Some("all"), done_since: Some(start_of_day(now)), limit: Some(1000), ..Default::default() };
    let tasks = board::list_tasks(db, &f, now)?;
    Ok(STATUSES.iter().map(|s| tasks.iter().filter(|t| t.status == *s).cloned().collect()).collect())
}

enum Action {
    Add,
    Block(i64),
    Title(i64),
    Due(i64),
    Delete(i64),
}

struct Prompt {
    label: String,
    value: String,
    confirm: bool,
    action: Action,
}

pub struct State {
    pub col: usize,
    rows: Vec<i64>,
    prompt: Option<Prompt>,
    pub facts: Option<Value>,
    pub msg: String,
    err: bool,
    pub quit: bool,
}

impl State {
    pub fn new() -> Self {
        State { col: 2, rows: vec![0; STATUSES.len()], prompt: None, facts: None, msg: String::new(), err: false, quit: false }
    }
}

/// Clamps the cursor to the board and returns the selected task, if any.
fn pick<'b>(s: &mut State, board: &'b [Vec<Task>]) -> Option<&'b Task> {
    for (r, col) in s.rows.iter_mut().zip(board) {
        *r = (*r).min(col.len() as i64 - 1).max(0);
    }
    board[s.col].get(s.rows[s.col] as usize)
}

fn follow(s: &mut State, db: &Connection, id: i64, now: i64) -> Result<()> {
    for (c, col) in load_board(db, now)?.iter().enumerate() {
        if let Some(r) = col.iter().position(|t| t.id == id) {
            s.col = c;
            s.rows[c] = r as i64;
        }
    }
    Ok(())
}

fn ask(s: &mut State, label: &str, value: &str, action: Action) {
    s.prompt = Some(Prompt { label: label.into(), value: value.into(), confirm: false, action });
}

fn change(s: &mut State, db: &Connection, id: i64, p: &Patch, what: &str, go: bool, now: i64) -> Result<()> {
    let t = board::update_task(db, &id.to_string(), p, now)?;
    if go {
        follow(s, db, t.id, now)?;
    }
    s.msg = format!("{} {}: {what}", fmt_id(t.id), t.title);
    Ok(())
}

/// Applies one key ("left", "return", "a", "\x03", …) to the state and the board. Errors become the status message.
pub fn press(s: &mut State, k: &str, db: &Connection, now: i64) {
    if k == "\x03" {
        s.quit = true;
        return;
    }
    s.msg.clear();
    s.err = false;
    let r = if s.prompt.is_some() {
        type_into(s, k, db, now)
    } else if s.facts.is_some() {
        s.facts = None;
        Ok(())
    } else {
        board_key(s, k, db, now)
    };
    if let Err(e) = r {
        s.msg = e.0;
        s.err = true;
        s.prompt = None;
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
        "escape" => s.prompt = None,
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
            Ok(())
        }
        Action::Block(id) => change(s, db, id, &Patch { status: Some("blocked"), blocked_reason: Some(v), ..Default::default() }, "→ blocked", false, now),
        Action::Title(id) => change(s, db, id, &Patch { title: Some(v), ..Default::default() }, "updated", false, now),
        Action::Due(_) if v.is_empty() => Ok(()),
        Action::Due(id) => change(s, db, id, &Patch { due: Some(v), ..Default::default() }, "updated", false, now),
        Action::Delete(id) => change(s, db, id, &Patch { delete: true, ..Default::default() }, "deleted", false, now),
    }
}

fn board_key(s: &mut State, k: &str, db: &Connection, now: i64) -> Result<()> {
    let board = load_board(db, now)?;
    let t = pick(s, &board).cloned();
    match k {
        "left" | "h" => s.col = s.col.saturating_sub(1),
        "right" | "l" => s.col = (s.col + 1).min(STATUSES.len() - 1),
        "up" | "k" => s.rows[s.col] -= 1,
        "down" | "j" => s.rows[s.col] += 1,
        "q" => s.quit = true,
        "s" => s.facts = Some(board::facts(db, "today", now)?),
        "a" => ask(s, "New task", "", Action::Add),
        _ => {
            let Some(t) = t else { return Ok(()) }; // the rest act on the selected task
            let i = FLOW.iter().position(|f| *f == if t.status == "blocked" { "doing" } else { &t.status }).unwrap_or(0);
            let to = |st| Patch { status: Some(st), ..Default::default() };
            match k {
                "<" if i > 0 => change(s, db, t.id, &to(FLOW[i - 1]), &format!("→ {}", FLOW[i - 1]), true, now)?,
                ">" if i + 1 < FLOW.len() => change(s, db, t.id, &to(FLOW[i + 1]), &format!("→ {}", FLOW[i + 1]), true, now)?,
                "d" => change(s, db, t.id, &to("done"), "→ done", false, now)?,
                "b" if t.status == "blocked" => change(s, db, t.id, &to("doing"), "→ doing", false, now)?,
                "b" => ask(s, "Blocked on", "", Action::Block(t.id)),
                "e" => ask(s, "Title", &t.title, Action::Title(t.id)),
                "t" => ask(s, "Due (fri, tomorrow 9:30, none)", "", Action::Due(t.id)),
                "p" => {
                    let p = (t.priority + 3) % 4;
                    change(s, db, t.id, &Patch { priority: Some(&p.to_string()), ..Default::default() }, &format!("priority {}", PRIORITY[p as usize]), false, now)?
                }
                "x" => s.prompt = Some(Prompt { label: format!("Delete {} {}? y/n", fmt_id(t.id), t.title), value: String::new(), confirm: true, action: Action::Delete(t.id) }),
                _ => {}
            }
        }
    }
    Ok(())
}

fn facts_lines(f: &Value) -> Vec<(String, &'static str)> {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let mut out = vec![(format!("Today so far ({})", s(&f["window"]["label"])), "1;4")];
    let mut list = |title: &str, items: &Value, show: &dyn Fn(&Value) -> String| {
        out.push((title.into(), "1"));
        let items = items.as_array().map(Vec::as_slice).unwrap_or_default();
        if items.is_empty() {
            out.push(("  none".into(), "2"));
        }
        out.extend(items.iter().map(|x| (format!("  {}", show(x)), "")));
    };
    let task = |t: &Value| format!("{} {}", s(&t["id"]), s(&t["title"]));
    for (title, key) in [("Done", "done"), ("Doing", "doing"), ("Due today", "due_today"), ("Overdue", "overdue")] {
        list(title, &f[key], &task);
    }
    list("Blocked", &f["blocked"], &|t| format!("{} {}: {}", s(&t["id"]), s(&t["title"]), s(&t["blocked_reason"])));
    list("Commits", &f["commits"], &|c| format!("{} {} {}", s(&c["project"]), s(&c["hash"]), s(&c["subject"])));
    out.push(("any key goes back".into(), "2"));
    out
}

pub struct View<'a> {
    pub width: usize,
    pub height: usize,
    pub now: i64,
    pub file: &'a str,
}

/// The whole screen as one string of ANSI: header, one column per status, the selected task, a message, then keys or a prompt.
pub fn render(s: &mut State, board: &[Vec<Task>], v: &View) -> String {
    let line = |text: &str, code: &str| paint(code, &fit(text, v.width));
    let today = start_of_day(v.now);
    let tomorrow = next_day(today);
    let late = |t: &Task| t.due_at.is_some_and(|d| d < today);
    let due_today = |t: &Task| t.due_at.is_some_and(|d| d >= today && d < tomorrow);
    let open: Vec<&Task> = board.iter().flatten().filter(|t| t.status != "done").collect();
    let mut head = vec![
        "huddle".to_string(),
        format!("{} open", open.len()),
        format!("{} overdue", open.iter().filter(|t| late(t)).count()),
        format!("{} due today", open.iter().filter(|t| due_today(t)).count()),
    ];
    if !v.file.is_empty() {
        head.push(v.file.into());
    }
    let mut out = vec![line(&head.join(" · "), "1")];

    if let Some(f) = &s.facts {
        out.extend(facts_lines(f).iter().map(|(t, c)| line(t, c)));
    } else {
        let w = v.width / STATUSES.len();
        let sel = pick(s, board);
        let body = v.height.saturating_sub(5).max(1);
        let top = (s.rows[s.col] as usize + 1).saturating_sub(body); // only the selected column scrolls
        out.push(STATUSES.iter().enumerate()
            .map(|(i, st)| paint(if i == s.col { "1;4" } else { "2" }, &fit(&format!(" {st} {}", board[i].len()), w)))
            .collect());
        for r in 0..body {
            out.push(board.iter().enumerate().map(|(i, col)| {
                let Some(x) = col.get(r + if i == s.col { top } else { 0 }) else { return " ".repeat(w) };
                let code = if sel.is_some_and(|t| t.id == x.id) { "7" }
                    else if x.status == "done" { "2" }
                    else if late(x) { "31" }
                    else if due_today(x) { "33" }
                    else if x.priority == 3 { "2" }
                    else { "" };
                paint(code, &fit(&format!(" {}{} {}", "!".repeat((2 - x.priority).max(0) as usize), fmt_id(x.id), x.title), w))
            }).collect());
        }
        out.push(line(&match sel {
            Some(t) => [
                Some(format!("{} {}", fmt_id(t.id), t.title)),
                t.project.clone(),
                t.due_at.map(|d| format!("due {}", fmt_when(d))),
                Some(PRIORITY[t.priority as usize].into()),
                t.blocked_reason.clone().filter(|b| !b.is_empty()).map(|b| format!("blocked: {b}")),
                Some(t.description.clone()).filter(|d| !d.is_empty()),
            ].into_iter().flatten().collect::<Vec<_>>().join(" · "),
            None => "empty column; a adds a task".into(),
        }, ""));
        out.push(line(&s.msg, if s.err { "31" } else { "32" }));
        out.push(match &s.prompt {
            Some(p) => line(&format!("{}: {}▏", p.label, p.value), "1"),
            None => line(KEYS, "2"),
        });
    }
    out.truncate(v.height);
    format!("\x1b[H{}\x1b[J", out.iter().map(|l| format!("{l}\x1b[K")).collect::<Vec<_>>().join("\r\n"))
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
        _ => String::new(),
    }
}

/// Raw mode and the alternate screen, undone on drop so an error still leaves a usable terminal.
struct Screen;

impl Screen {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let screen = Screen;
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        Ok(screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

pub fn main() -> Result<()> {
    let file = board::db_file();
    let db = board::open_db(&file)?;
    let mut s = State::new();
    let _screen = Screen::enter()?;
    let mut stdout = io::stdout();
    while !s.quit {
        let (w, h) = terminal::size().unwrap_or((100, 30));
        let frame = render(&mut s, &load_board(&db, now())?, &View { width: w as usize, height: h as usize, now: now(), file: &file });
        stdout.write_all(frame.as_bytes())?;
        stdout.flush()?;
        // redraw every 2s anyway, to pick up what Claude changes while the board is open
        if event::poll(Duration::from_secs(2))? {
            if let Event::Key(e) = event::read()? {
                if e.kind != KeyEventKind::Release {
                    press(&mut s, &key_name(&e), &db, now());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::at;

    fn keys(s: &mut State, db: &Connection, ks: &[&str]) {
        for k in ks {
            press(s, k, db, now());
        }
    }
    fn chars(text: &str) -> Vec<String> {
        text.chars().map(String::from).collect()
    }
    fn plain(screen: &str) -> Vec<String> {
        let mut out = String::new();
        let mut esc = false;
        for c in screen.chars() {
            match c {
                '\x1b' => esc = true,
                c if esc => esc = !c.is_ascii_alphabetic(),
                c => out.push(c),
            }
        }
        out.split("\r\n").map(String::from).collect()
    }
    fn task(db: &Connection) -> Task {
        board::list_tasks(db, &Filter { status: Some("all"), ..Default::default() }, now()).unwrap().remove(0)
    }

    #[test]
    fn board_has_a_column_per_status_done_shows_only_today_and_lines_fit() {
        let db = board::open_db(":memory:").unwrap();
        let mon = at("2026-10-26T10:00");
        let wed = at("2026-10-28T10:00");
        let add = |title, status, due, at| board::add_task(&db, &NewTask { title, status, due, ..Default::default() }, at).unwrap();
        add("Old win", Some("done"), None, mon);
        add("Fresh win", None, None, mon);
        board::update_task(&db, "#2", &Patch { status: Some("done"), ..Default::default() }, wed).unwrap();
        add("Late thing", None, Some("2026-10-27"), mon);

        let b = load_board(&db, wed).unwrap();
        assert_eq!(b[5].iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Fresh win"]);
        let lines = plain(&render(&mut State::new(), &b, &View { width: 90, height: 12, now: wed, file: "" }));
        assert_eq!(lines.len(), 12);
        assert!(lines[0].contains("1 open · 1 overdue · 0 due today"), "{}", lines[0]);
        assert!(lines.iter().any(|l| l.contains("#3 Late thing")));
        assert!(!lines.iter().any(|l| l.contains("Old win")));
        assert!(lines.iter().all(|l| l.chars().count() <= 90));
    }

    #[test]
    fn keys_add_move_block_unblock_edit_delete() {
        let db = board::open_db(":memory:").unwrap();
        let mut s = State::new(); // starts on doing
        let typed = |text: &str, before: &[&str], after: &[&str]| -> Vec<String> {
            before.iter().map(|k| k.to_string()).chain(chars(text)).chain(after.iter().map(|k| k.to_string())).collect()
        };
        let run = |s: &mut State, ks: Vec<String>| keys(s, &db, &ks.iter().map(String::as_str).collect::<Vec<_>>());

        run(&mut s, typed("Write docs", &["a"], &["return"]));
        assert_eq!(task(&db).status, "doing");
        keys(&mut s, &db, &[">"]);
        assert_eq!(task(&db).status, "review");
        assert_eq!(STATUSES[s.col], "review"); // the cursor follows the task

        keys(&mut s, &db, &["b", "return"]);
        assert!(s.msg.contains("reason"));
        run(&mut s, typed("no keys", &["b"], &["return"]));
        assert_eq!(task(&db).blocked_reason.as_deref(), Some("no keys"));
        keys(&mut s, &db, &["right", "b"]);
        assert_eq!(task(&db).status, "doing");

        run(&mut s, typed("tests", &["left", "left", "e", "backspace", "backspace", "backspace", "backspace"], &["return", "p"]));
        assert_eq!(task(&db).title, "Write tests");
        assert_eq!(task(&db).priority, 1);
        assert!(plain(&render(&mut s, &load_board(&db, now()).unwrap(), &View { width: 120, height: 20, now: now(), file: "" }))[1].contains("doing 1"));

        keys(&mut s, &db, &["x", "n"]);
        assert_eq!(board::list_tasks(&db, &Filter::default(), now()).unwrap().len(), 1);
        keys(&mut s, &db, &["x", "y"]);
        assert_eq!(board::list_tasks(&db, &Filter::default(), now()).unwrap().len(), 0);

        keys(&mut s, &db, &["s"]);
        assert!(plain(&render(&mut s, &[], &View { width: 120, height: 20, now: now(), file: "" })).iter().any(|l| l.starts_with("Today so far")));
        keys(&mut s, &db, &["q"]);
        assert!(s.facts.is_none() && !s.quit); // any key closes the facts view first
        keys(&mut s, &db, &["q"]);
        assert!(s.quit);
    }
}
