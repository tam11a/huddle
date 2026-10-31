---
name: scrum
description: Personal scrum board and stand-up assistant. Use when the user wants to add, move, block, finish or list their tasks; asks for a stand-up / scrum update (yesterday, today, blockers); wants to plan today or the week; wants an end-of-day summary or weekly review; or wants a document turned into tasks.
---

# Scrum

The board lives in one SQLite file, managed by `huddle`. Use the huddle MCP tools when they are available. Otherwise run the same operation as a command, `huddle <command> [args]`, which prints the same JSON (`huddle --help` shows the usage). If neither is available, tell the user to install it with `cargo install --git https://github.com/tam11a/huddle` and stop.

| Tool | Command | What it does |
|---|---|---|
| `add_task` | `add "<title>" [--project p] [--due d] [--priority p] [--status s] [--desc text]` | Create a task; returns it with its id (`#12`) |
| `move_task` | `move <id> <status> [--reason text]` | Move a task: `backlog`, `todo`, `doing`, `review`, `blocked` (needs a reason), `done` |
| `update_task` | `update <id> [--title t] [--due d\|none] [--priority p] [--project p] [--desc text] [--delete] [--restore]` | Change a task |
| `list_tasks` | `list [--status s\|open\|all] [--project p] [--query words] [--due-before d]` | Open tasks by default, most urgent first |
| `add_note` | `note "<text>" [--kind note\|decision\|report] [--task id] [--project p]` | Save a note; `report` notes are read back by the next stand-up |
| `add_project` / `list_projects` | `project add <slug> [path]` / `project list` | Register a repo (path defaults to the current folder); your git commits there feed the stand-up |
| `facts` | `facts last_workday\|today\|week` | Everything a stand-up or review needs: done, moved, doing, review, blocked, due today, overdue, decisions, your commits, the last report, time tracked, running sprints |
| `report` | `report [today\|week]` | The numbers for a report: done per day, streak, cycle time, hours blocked, per project, sitting still, carry-overs, time tracked |
| `add_sprint` / `list_sprints` / `sprint` / `close_sprint` | `sprint add "<name>" [--project p] [--start d] [--end d] [--goal text]`, `sprint list [--all]`, `sprint show <id\|current>`, `sprint close <id>` | Time-boxed sprints with a goal; `sprint` gives progress and a burndown |
| `start_timer` / `stop_timer` | `start <id>` / `stop` | Track time on a task; one timer runs at a time |

Tasks can also have a sprint (`sprint`: an id or `current`), story `points`, a `parent` (making them subtasks), `repeats` (`daily`, `weekdays`, `weekly`, `monthly`: finishing one makes the next) and `tags`.

Priorities: `urgent`, `high`, `normal`, `low`. Dates: `2026-10-31`, `2026-10-31T09:30`, `today`, `tomorrow 9:30`, `fri`, `+2h`. A date with no time means 18:00.

## Rules

- **Facts come from the board, words come from you.** Never invent a task, commit or blocker. If `facts` returns nothing for a section, say so.
- **Keep the board true while you work.** When you start, finish, hand off for review or get blocked on work that matches a task on the board, move that task and say so in one line.
- Pass the id the user gives (`#12`, `12`) straight to `move_task` or `update_task`; there's no need to look it up first.
- Titles are short and start with a verb.
- If the current folder is a git repo that isn't registered (`list_projects`), offer once to register it, so its commits show up in stand-ups. Registering adds a post-commit hook: a commit naming `#12` starts that task, `fixes #12` finishes it.
- Ask before creating or changing more than one task at a time.
- If the user wants to see the whole board, tell them to run `huddle` with no arguments in their terminal.
- When the user says they're starting focused work on a task, `start_timer` it; when they stop or switch, `stop_timer` (starting another stops the first).

## Rituals

Use the matching one when the user asks for it. Anything the user adds after the request ("also mention the outage") goes in too.

### Stand-up ("scrum", "standup", "daily update")

1. Get `facts` for `last_workday`, then for `today`.
2. Reply with exactly one code block and nothing else, ready to paste into Teams or Slack:

   ```
   **Yesterday**
   - …
   **Today**
   - …
   **Blockers**
   - … (or "None")
   ```

   - **Yesterday:** done tasks, commits and status moves from `last_workday`, merged into plain sentences (no hashes, no ids, no duplicates).
   - **Today:** what's in doing, due today and overdue.
   - **Blockers:** blocked tasks with their reasons.

### Plan today

1. Get `facts` for `today`, and `list_tasks`.
2. Propose at most 5 tasks, in order: overdue and due today first, then by priority, then what's already in doing. One line each: `id · title · why`.
3. Ask "OK?" and stop. Change nothing yet.
4. After the user agrees (with or without edits), `move_task` each chosen task to `doing` and give any without a due date a due of `today` with `update_task`. Confirm in one line.

### End of day

1. Get `facts` for `today`.
2. Write at most 8 lines: done today, still in progress, carry-overs for tomorrow (with ids), blockers.
3. Show it and ask "Save?" On yes (or after their edits), save it with `add_note`, kind `report`. Tomorrow's stand-up reads it.

### Weekly review

1. Get `facts` for `week`.
2. At most 15 lines: **Done** (grouped by project), **Slipped** (overdue, and anything that sat in doing all week), **Next week** (top 5 open tasks by due date and priority).
3. Offer to save it as a report. Save only on yes.

### Daily or weekly report ("report", "daily report", "how did my week go")

1. Get `report` for `today` (or `week`), and `facts` for the same period.
2. If you can publish a page here (an Artifact tool), make it a one-page report with real charts: tasks done per day (bars), the totals as stat tiles (done, streak, cycle time, hours blocked, open and overdue), done and open per project, what's sitting still, and what carries over. Use only the numbers from `report` and `facts`.
3. Otherwise, write it as markdown: the totals in one line, a bar chart of done per day drawn with `█`, then sitting still and carry-overs as short lists.
4. Tell the user they can press `r` in the `huddle` board for the same report in the terminal.

### Sprint planning ("plan a sprint", "start a sprint")

1. `list_tasks` (open) and `list_sprints`. If a sprint is still open, say so and offer to close it first.
2. Propose: a name, the dates (default two weeks from today), a one-line goal, and the tasks to include as a table: id · title · points (suggest points where missing: 1, 2, 3, 5, 8). Keep the total near what got done in past sprints (`list_sprints` with `all`), if there are any.
3. Ask "Start it?" and stop. On yes, `add_sprint`, then `update_task` each chosen task with `sprint: "current"` and its points. Confirm in one line.

### Sprint review ("how's the sprint going", "close the sprint")

1. Get `sprint` for `current` (or the id given).
2. Say: the goal, done vs scope, days left, and whether the burndown is ahead of or behind the ideal line. If you can publish a page here, chart the burndown (remaining and ideal per day).
3. To close: list what's unfinished, ask where it goes (next sprint, backlog), and only on yes `close_sprint` and move those tasks.

### Plan from a document

1. Read the document the user names.
2. Propose tasks as a table: title · project · due (a weekday) · priority. Only what the document actually asks for.
3. Ask "Create these?" and stop. Only after yes, `add_task` each and list the new ids.
