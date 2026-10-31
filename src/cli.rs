// The commands Claude runs. Each prints JSON (or the usage text).
use crate::board::{self, Error, Filter, NewTask, Patch, Result, err};
use crate::time::now;
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;

pub enum Out {
    Text(String),
    Json(Value),
}

pub fn usage() -> String {
    format!(r##"huddle [<command> [args]]   (prints JSON; board file: {})
  (no command, in a terminal)                 open the interactive board
  mcp                                         serve these commands as MCP tools over stdio (Claude connects to it)
  add "<title>" [--project p] [--due d] [--priority urgent|high|normal|low] [--status s] [--desc text]
      [--sprint id|current] [--points n] [--parent id] [--every daily|weekdays|weekly|monthly] [--tags a,b]
  move <id> <status> [--reason text]          statuses: {} (blocked needs --reason)
  update <id> [--title t] [--due d|none] [--priority p] [--project p] [--desc text] [--delete] [--restore]
      [--sprint id|current|none] [--points n|none] [--parent id|none] [--every rule|none] [--tags a,b]   (--tags replaces; "" clears)
  list [--status s|open|all] [--project p] [--query words] [--due-before d] [--sprint id|current] [--tag t] [--parent id]
  sprint add "<name>" [--project p] [--start d] [--end d|+14d] [--goal text]   (defaults: today, for two weeks)
  sprint list [--all] | sprint show <id|current> | sprint close <id|current>
  start <id>                   track time on a task (stops any running timer; backlog/todo moves to doing)
  stop                         stop the running timer
  note "<text>" [--kind note|decision|report] [--task id] [--project p]
  project add <slug> [path] [--no-hook]   (path defaults to the current folder; its git commits feed the stand-up,
                               and a post-commit hook moves tasks named in commits: "#12" starts it, "fixes #12" finishes it)
  project list
  facts last_workday|today|week
  report [today|week]          numbers for a report: done per day, streak, cycle time, time blocked, what sits still, carry-overs
dates: 2026-10-31, 2026-10-31T09:30, today, tomorrow 9:30, fri, +2h"##, board::db_file(), board::STATUSES.join(", "))
}

const VALUES: [&str; 20] = ["project", "due", "priority", "status", "desc", "reason", "title", "query", "due-before", "kind", "task",
    "sprint", "points", "parent", "every", "tags", "tag", "start", "end", "goal"];
const FLAGS: [&str; 5] = ["delete", "restore", "help", "no-hook", "all"];

/// Splits argv into positionals and --options, the way the Node version's parseArgs did.
fn parse(args: &[String]) -> Result<(Vec<String>, HashMap<String, String>)> {
    let (mut pos, mut opt) = (vec![], HashMap::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let Some(name) = a.strip_prefix("--").or(if a == "-h" { Some("help") } else { None }) else {
            pos.push(a.clone());
            continue;
        };
        if name.is_empty() {
            pos.extend(it.by_ref().cloned());
            break;
        }
        let (name, inline) = name.split_once('=').map_or((name, None), |(n, v)| (n, Some(v.to_string())));
        if VALUES.contains(&name) {
            let v = inline.or_else(|| it.next().cloned()).ok_or_else(|| Error(format!("--{name} needs a value")))?;
            opt.insert(name.to_string(), v);
        } else if FLAGS.contains(&name) && inline.is_none() {
            opt.insert(name.to_string(), String::new());
        } else {
            return err(format!("unknown option --{name}\n\n{}", usage()));
        }
    }
    Ok((pos, opt))
}

fn need<'a>(v: Option<&'a str>, what: &str) -> Result<&'a str> {
    v.filter(|v| !v.is_empty()).ok_or_else(|| Error(format!("missing {what}\n\n{}", usage())))
}

pub fn run(args: &[String], db: &Connection) -> Result<Out> {
    let (p, o) = parse(args)?;
    let o = |k: &str| o.get(k).map(String::as_str);
    let arg = |i: usize| p.get(i).map(String::as_str);
    let rest = p.get(1..).unwrap_or_default().join(" ");
    let rest = rest.as_str();
    let json = |v: Value| Ok(Out::Json(v));
    let task = |t: board::Task| json(t.view());

    match if o("help").is_some() { Some("help") } else { arg(0) } {
        Some("add") => task(board::add_task(db, &NewTask {
            title: need(Some(rest), "title")?, project: o("project"), due: o("due"), priority: o("priority"), status: o("status"), description: o("desc"),
            sprint: o("sprint"), points: o("points"), parent: o("parent"), recur: o("every"), tags: o("tags"),
        }, now())?),
        Some("move") => task(board::update_task(db, need(arg(1), "task id")?, &Patch {
            status: Some(need(arg(2), "status")?), blocked_reason: o("reason"), ..Default::default()
        }, now())?),
        Some("update") => task(board::update_task(db, need(arg(1), "task id")?, &Patch {
            title: o("title"), due: o("due"), priority: o("priority"), project: o("project"), status: o("status"),
            blocked_reason: o("reason"), description: o("desc"), delete: o("delete").is_some(), restore: o("restore").is_some(),
            sprint: o("sprint"), points: o("points"), parent: o("parent"), recur: o("every"), tags: o("tags"),
        }, now())?),
        Some("list") => json(board::list_tasks(db, &Filter {
            status: o("status"), project: o("project"), query: o("query"), due_before: o("due-before"), limit: Some(50),
            sprint: o("sprint"), tag: o("tag"), parent: o("parent"), ..Default::default()
        }, now())?.iter().map(board::Task::view).collect()),
        Some("note") => json(board::add_note(db, need(Some(rest), "note text")?, o("kind"), o("task"), o("project"), now())?),
        Some("project") => match arg(1) {
            Some("add") => {
                let path = std::path::absolute(arg(3).unwrap_or("."))?;
                json(crate::hooks::add_project(db, need(arg(2), "project slug")?, &path, o("no-hook").is_none())?)
            }
            Some("list") => json(board::list_projects(db)?.into_iter().map(|(slug, path)| serde_json::json!({ "slug": slug, "path": path })).collect()),
            _ => err(format!("project add|list\n\n{}", usage())),
        },
        Some("sprint") => match arg(1) {
            Some("add") => json(board::add_sprint(db, need(Some(p.get(2..).unwrap_or_default().join(" ").as_str()), "sprint name")?, o("project"), o("start"), o("end"), o("goal"), now())?),
            Some("list") => json(board::list_sprints(db, o("all").is_some(), now())?.into()),
            Some("show") => json(board::sprint_detail(db, arg(2).unwrap_or("current"), now())?),
            Some("close") => json(board::close_sprint(db, need(arg(2), "sprint id")?, now())?),
            _ => err(format!("sprint add|list|show|close\n\n{}", usage())),
        },
        Some("start") => json(board::start_timer(db, need(arg(1), "task id")?, now())?),
        Some("stop") => json(board::stop_timer(db, now())?),
        Some("facts") => json(board::facts(db, need(arg(1), "period")?, now())?),
        Some("report") => json(board::report(db, arg(1).unwrap_or("today"), now())?),
        Some("hook") => Ok(Out::Text(crate::hooks::run(db, arg(1), now())?)),
        Some("help") | None => Ok(Out::Text(usage())),
        Some(cmd) => err(format!("unknown command \"{cmd}\"\n\n{}", usage())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_(db: &Connection, args: &[&str]) -> Result<Value> {
        match run(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>(), db)? {
            Out::Json(v) => Ok(v),
            Out::Text(t) => Ok(Value::String(t)),
        }
    }

    #[test]
    fn run_parses_commands() {
        let db = board::open_db(":memory:").unwrap();
        assert_eq!(run_(&db, &["add", "Write", "docs", "--priority", "low"]).unwrap()["title"], "Write docs");
        assert_eq!(run_(&db, &["move", "#1", "review"]).unwrap()["status"], "review");
        assert_eq!(run_(&db, &["list", "--status=review"]).unwrap().as_array().unwrap().len(), 1);
        assert!(run_(&db, &["move", "#1"]).unwrap_err().0.contains("missing status"));
        assert!(run_(&db, &["list", "--bogus"]).unwrap_err().0.contains("unknown option"));
        assert!(run_(&db, &["--help"]).unwrap().as_str().unwrap().starts_with("huddle"));
    }
}
