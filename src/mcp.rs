// MCP server over stdio: the same board, as tools Claude calls directly. Newline-delimited JSON-RPC 2.0.
use crate::board::{self, Error, Filter, NewTask, PERIODS, Patch, Result, STATUSES, Task, err};
use crate::time::now;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

const PROTOCOL: &str = "2025-06-18";
const DATES: &str = "2026-10-31, 2026-10-31T09:30, today, tomorrow 9:30, fri, +2h. A date with no time means 18:00.";
const INSTRUCTIONS: &str = "Huddle is the user's personal scrum board. Facts come from the board, words come from you: \
never invent a task, commit or blocker. When you start, finish, hand off for review or get blocked on work that matches \
a task on the board, move that task and say so in one line. Ask before creating or changing more than one task at a time.";

pub fn serve(db: &Connection) -> Result<()> {
    let mut out = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(db, &line) {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// One JSON-RPC message in, at most one reply out: notifications get none.
pub fn handle(db: &Connection, line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } })),
    };
    let id = msg.get("id")?.clone();
    let params = &msg["params"];
    let result = match msg["method"].as_str().unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "huddle", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => match call(db, params["name"].as_str().unwrap_or(""), &params["arguments"]) {
            Ok(v) => json!({ "content": [{ "type": "text", "text": v.to_string() }] }),
            Err(e) => json!({ "content": [{ "type": "text", "text": e.0 }], "isError": true }),
        },
        m => return Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("unknown method {m}") } })),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn call(db: &Connection, name: &str, a: &Value) -> Result<Value> {
    // arguments arrive as JSON; the board takes strings, so numbers and bools are accepted as their text
    let s = |k: &str| match &a[k] {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let need = |k: &str| s(k).filter(|v| !v.is_empty()).ok_or_else(|| Error(format!("missing {k}")));
    let flag = |k: &str| a[k].as_bool().unwrap_or(false);
    let (project, due, priority, status, description) = (s("project"), s("due"), s("priority"), s("status"), s("description"));
    let (sprint, points, parent, repeats) = (s("sprint"), s("points"), s("parent"), s("repeats"));
    // tags may come as ["a", "b"] or "a, b"
    let tags = match &a["tags"] {
        Value::Array(t) => Some(t.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")),
        _ => s("tags"),
    };
    let n = now();
    Ok(match name {
        "add_task" => board::add_task(db, &NewTask {
            title: &need("title")?, project: project.as_deref(), due: due.as_deref(), priority: priority.as_deref(),
            status: status.as_deref(), description: description.as_deref(),
            sprint: sprint.as_deref(), points: points.as_deref(), parent: parent.as_deref(), recur: repeats.as_deref(), tags: tags.as_deref(),
        }, n)?.view(),
        "move_task" => board::update_task(db, &need("id")?, &Patch {
            status: Some(&need("status")?), blocked_reason: s("reason").as_deref(), ..Default::default()
        }, n)?.view(),
        "update_task" => board::update_task(db, &need("id")?, &Patch {
            title: s("title").as_deref(), due: due.as_deref(), priority: priority.as_deref(), project: project.as_deref(),
            status: status.as_deref(), blocked_reason: s("reason").as_deref(), description: description.as_deref(),
            delete: flag("delete"), restore: flag("restore"),
            sprint: sprint.as_deref(), points: points.as_deref(), parent: parent.as_deref(), recur: repeats.as_deref(), tags: tags.as_deref(),
        }, n)?.view(),
        "list_tasks" => board::list_tasks(db, &Filter {
            status: status.as_deref(), project: project.as_deref(), query: s("query").as_deref(), due_before: s("due_before").as_deref(),
            limit: Some(50), sprint: sprint.as_deref(), tag: s("tag").as_deref(), parent: parent.as_deref(), ..Default::default()
        }, n)?.iter().map(Task::view).collect(),
        "add_note" => board::add_note(db, &need("content")?, s("kind").as_deref(), s("task").as_deref(), project.as_deref(), n)?,
        "add_project" => {
            let path = match s("path").filter(|p| !p.is_empty()) {
                Some(p) => std::path::absolute(p)?,
                None => std::env::current_dir()?,
            };
            crate::hooks::add_project(db, &need("slug")?, &path, a["hook"].as_bool() != Some(false))?
        }
        "list_projects" => board::list_projects(db)?.into_iter().map(|(slug, path)| json!({ "slug": slug, "path": path })).collect(),
        "facts" => board::facts(db, &need("period")?, n)?,
        "report" => board::report(db, s("period").as_deref().unwrap_or("today"), n)?,
        "add_sprint" => board::add_sprint(db, &need("name")?, project.as_deref(), s("start").as_deref(), s("end").as_deref(), s("goal").as_deref(), n)?,
        "list_sprints" => board::list_sprints(db, flag("all"), n)?.into(),
        "sprint" => board::sprint_detail(db, sprint.as_deref().unwrap_or("current"), n)?,
        "close_sprint" => board::close_sprint(db, &need("sprint")?, n)?,
        "start_timer" => board::start_timer(db, &need("id")?, n)?,
        "stop_timer" => board::stop_timer(db, n)?,
        _ => return err(format!("unknown tool {name}")),
    })
}

fn tools() -> Value {
    let text = |d: &str| json!({ "type": "string", "description": d });
    let priority = json!({ "type": "string", "enum": ["urgent", "high", "normal", "low"] });
    let id = text("Task id: #12, 12 or O-12");
    let read_only = json!({ "readOnlyHint": true });
    let list_status = [STATUSES.as_slice(), &["open", "all"]].concat();
    let sprint = text("A sprint id (#3) or \"current\" for the one running now");
    let points = json!({ "type": ["integer", "string"], "description": "Story points, a whole number; \"none\" clears" });
    let parent = text("The parent task's id, which makes this a subtask; \"none\" clears");
    let repeats = json!({ "type": "string", "enum": ["daily", "weekdays", "weekly", "monthly", "none"], "description": "Finishing it makes the next copy, due at the next turn" });
    let tags = json!({ "type": "array", "items": { "type": "string" }, "description": "Tags, like [\"frontend\", \"bug\"]" });
    json!([
        {
            "name": "add_task",
            "description": "Create a task on the board. Returns it with its id (#12).",
            "inputSchema": { "type": "object", "required": ["title"], "properties": {
                "title": text("Short, starts with a verb"),
                "project": text("A registered project slug"),
                "due": text(DATES),
                "priority": priority,
                "status": { "type": "string", "enum": STATUSES, "description": "Defaults to todo" },
                "description": text("Longer notes"),
                "sprint": sprint,
                "points": points,
                "parent": parent,
                "repeats": repeats,
                "tags": tags,
            }},
        },
        {
            "name": "move_task",
            "description": "Move a task to another status: when work on it starts (doing), goes to review, gets blocked (needs a reason) or is finished (done).",
            "inputSchema": { "type": "object", "required": ["id", "status"], "properties": {
                "id": id,
                "status": { "type": "string", "enum": STATUSES },
                "reason": text("Why it is blocked; required when status is blocked"),
            }},
        },
        {
            "name": "update_task",
            "description": "Change a task's title, due date, priority, project or description, or delete or restore it.",
            "inputSchema": { "type": "object", "required": ["id"], "properties": {
                "id": id,
                "title": text("New title"),
                "due": text(&format!("{DATES} \"none\" clears it.")),
                "priority": priority,
                "project": text("A registered project slug; empty clears it"),
                "description": text("Longer notes"),
                "delete": { "type": "boolean" },
                "restore": { "type": "boolean" },
                "sprint": text("A sprint id, \"current\", or \"none\" to take it out of its sprint"),
                "points": points,
                "parent": parent,
                "repeats": repeats,
                "tags": { "type": "array", "items": { "type": "string" }, "description": "Replaces the tags; [] clears them" },
            }},
        },
        {
            "name": "list_tasks",
            "description": "Tasks on the board, most urgent first. Open tasks (everything not done) by default.",
            "annotations": read_only,
            "inputSchema": { "type": "object", "properties": {
                "status": { "type": "string", "enum": list_status },
                "project": text("A registered project slug"),
                "query": text("Words in the title or description, or a task id"),
                "due_before": text(DATES),
                "sprint": sprint,
                "tag": text("Only tasks with this tag"),
                "parent": text("Only the subtasks of this task"),
            }},
        },
        {
            "name": "add_note",
            "description": "Save a note. Decisions show up in stand-ups; a report is an end-of-day or weekly summary that the next stand-up reads back.",
            "inputSchema": { "type": "object", "required": ["content"], "properties": {
                "content": text("The note"),
                "kind": { "type": "string", "enum": ["note", "decision", "report"], "description": "Defaults to note" },
                "task": id,
                "project": text("A registered project slug"),
            }},
        },
        {
            "name": "add_project",
            "description": "Register a git repo so the user's commits there show up in stand-ups. It also adds a post-commit hook: a commit naming #12 starts that task, \"fixes #12\" finishes it.",
            "inputSchema": { "type": "object", "required": ["slug"], "properties": {
                "slug": text("Short name: a-z, 0-9, . _ -"),
                "path": text("The repo folder; defaults to the folder Claude is working in"),
                "hook": { "type": "boolean", "description": "Add the post-commit hook; defaults to true" },
            }},
        },
        {
            "name": "list_projects",
            "description": "The registered projects and their folders.",
            "annotations": read_only,
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "facts",
            "description": "Everything a stand-up, plan, end-of-day or weekly review needs: done, moved, doing, review, blocked, due today, overdue, decisions, the user's commits and the last report.",
            "annotations": read_only,
            "inputSchema": { "type": "object", "required": ["period"], "properties": {
                "period": { "type": "string", "enum": PERIODS, "description": "last_workday is the most recent day with any activity" },
            }},
        },
        {
            "name": "report",
            "description": "The numbers for a daily or weekly report with charts: totals, tasks done on each of the last 14 days, workday streak, cycle time (doing → done), hours blocked, per-project counts, tasks sitting still in doing/review/blocked, and what carries over.",
            "annotations": read_only,
            "inputSchema": { "type": "object", "properties": {
                "period": { "type": "string", "enum": ["today", "week"], "description": "Defaults to today" },
            }},
        },
        {
            "name": "add_sprint",
            "description": "Start a sprint: a named, time-boxed set of tasks with a goal. Put tasks in it with add_task or update_task (sprint).",
            "inputSchema": { "type": "object", "required": ["name"], "properties": {
                "name": text("Like \"Sprint 12\" or \"Checkout v2\""),
                "project": text("A registered project slug"),
                "start": text(&format!("First day; defaults to today. {DATES}")),
                "end": text("Last day, a date or +14d from the start; defaults to two weeks"),
                "goal": text("What the sprint should achieve"),
            }},
        },
        {
            "name": "list_sprints",
            "description": "Open sprints (or all with all=true), with progress: done and scope (points, or tasks when nothing has points), days left, state.",
            "annotations": read_only,
            "inputSchema": { "type": "object", "properties": { "all": { "type": "boolean" } } },
        },
        {
            "name": "sprint",
            "description": "One sprint in detail: progress, its open tasks, and a burndown (remaining work at the end of each day, with the ideal line). Good for sprint charts and reviews.",
            "annotations": read_only,
            "inputSchema": { "type": "object", "properties": { "sprint": text("A sprint id, or \"current\" (the default)") } },
        },
        {
            "name": "close_sprint",
            "description": "Close a sprint. Its unfinished tasks are listed in the answer, to move to the next sprint or back to the backlog.",
            "inputSchema": { "type": "object", "required": ["sprint"], "properties": { "sprint": sprint } },
        },
        {
            "name": "start_timer",
            "description": "Track time on a task (stops any running timer). A task in backlog or todo moves to doing. Use when the user starts focused work on a task.",
            "inputSchema": { "type": "object", "required": ["id"], "properties": { "id": id } },
        },
        {
            "name": "stop_timer",
            "description": "Stop the running timer; returns this session's time and the task's total.",
            "inputSchema": { "type": "object", "properties": {} },
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(db: &Connection, method: &str, params: Value) -> Value {
        handle(db, &json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string()).unwrap()
    }
    fn tool(db: &Connection, name: &str, args: Value) -> (bool, Value) {
        let r = rpc(db, "tools/call", json!({ "name": name, "arguments": args }))["result"].clone();
        let text = r["content"][0]["text"].as_str().unwrap().to_string();
        (r["isError"] != true, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    #[test]
    fn handshake_lists_tools_and_ignores_notifications() {
        let db = board::open_db(":memory:").unwrap();
        let init = rpc(&db, "initialize", json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "t" } }));
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "huddle");
        assert_eq!(handle(&db, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), None);
        let names: Vec<String> = rpc(&db, "tools/list", json!({}))["result"]["tools"].as_array().unwrap()
            .iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["add_task", "move_task", "update_task", "list_tasks", "add_note", "add_project", "list_projects", "facts", "report",
            "add_sprint", "list_sprints", "sprint", "close_sprint", "start_timer", "stop_timer"]);
        assert_eq!(rpc(&db, "nope", json!({}))["error"]["code"], -32601);
        assert_eq!(handle(&db, "{not json").unwrap()["error"]["code"], -32700);
    }

    #[test]
    fn tools_change_the_board_and_errors_come_back_as_tool_errors() {
        let db = board::open_db(":memory:").unwrap();
        let (ok, t) = tool(&db, "add_task", json!({ "title": "Write docs", "priority": "high", "due": "2026-11-02" }));
        assert!(ok);
        assert_eq!((t["id"].as_str(), t["priority"].as_i64()), (Some("#1"), Some(1)));
        let (ok, e) = tool(&db, "move_task", json!({ "id": 1, "status": "blocked" }));
        assert!(!ok && e.as_str().unwrap().contains("reason"));
        assert_eq!(tool(&db, "move_task", json!({ "id": "#1", "status": "blocked", "reason": "no keys" })).1["blocked_reason"], "no keys");
        assert_eq!(tool(&db, "list_tasks", json!({ "status": "blocked" })).1.as_array().unwrap().len(), 1);
        assert!(tool(&db, "add_note", json!({ "content": "Use Postgres", "kind": "decision" })).0);
        std::thread::sleep(std::time::Duration::from_millis(2)); // facts' window ends just before now
        assert_eq!(tool(&db, "facts", json!({ "period": "today" })).1["decisions"][0], "Use Postgres");
        assert!(!tool(&db, "add_task", json!({})).0);
        assert!(!tool(&db, "fly", json!({})).0);
    }
}
