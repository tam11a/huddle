---
name: scrum
description: Personal scrum board and stand-up assistant. Use when the user wants to add, move, block, finish or list their tasks; asks for a stand-up / scrum update (yesterday, today, blockers); wants to plan today or the week; wants an end-of-day summary or weekly review; or wants a document turned into tasks.
---

# Scrum

The board lives in one SQLite file, managed by the `huddle` command:

```bash
huddle <command> [args]
```

It prints JSON. Run `huddle --help` for the full usage. If `huddle` isn't installed, tell the user to install it with `cargo install --git https://github.com/tam11a/huddle` and stop. The commands:

| Command | What it does |
|---|---|
| `add "<title>" [--project p] [--due d] [--priority urgent\|high\|normal\|low] [--status s] [--desc text]` | Create a task; returns it with its id (`#12`) |
| `move <id> <status> [--reason text]` | Move a task: `backlog`, `todo`, `doing`, `review`, `blocked` (needs `--reason`), `done` |
| `update <id> [--title t] [--due d\|none] [--priority p] [--project p] [--desc text] [--delete] [--restore]` | Change a task |
| `list [--status s\|open\|all] [--project p] [--query words] [--due-before d]` | Open tasks by default, most urgent first |
| `note "<text>" [--kind note\|decision\|report] [--task id] [--project p]` | Save a note; `report` notes are read back by the next stand-up |
| `project add <slug> [path]` / `project list` | Register a repo (path defaults to the current folder); your git commits there feed the stand-up |
| `facts last_workday\|today\|week` | Everything a stand-up or review needs: done, moved, doing, review, blocked, due today, overdue, decisions, your commits, the last report |

Dates: `2026-10-31`, `2026-10-31T09:30`, `today`, `tomorrow 9:30`, `fri`, `+2h`. A date with no time means 18:00.

## Rules

- **Facts come from the board, words come from you.** Never invent a task, commit or blocker. If `facts` returns nothing for a section, say so.
- Pass the id the user gives (`#12`, `12`) straight to `move` or `update`; there's no need to look it up first.
- Titles are short and start with a verb.
- If the current folder is a git repo that isn't registered (`project list`), offer once to register it, so its commits show up in stand-ups.
- Ask before creating or changing more than one task at a time.
- If the user wants to see the whole board, tell them to run `huddle` with no arguments in their terminal.

## Rituals

Use the matching one when the user asks for it. Anything the user adds after the request ("also mention the outage") goes in too.

### Stand-up ("scrum", "standup", "daily update")

1. Run `facts last_workday`, then `facts today`.
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

1. Run `facts today` and `list`.
2. Propose at most 5 tasks, in order: overdue and due today first, then by priority, then what's already in doing. One line each: `id · title · why`.
3. Ask "OK?" and stop. Change nothing yet.
4. After the user agrees (with or without edits), `move` each chosen task to `doing` and give any without a due date `--due today`. Confirm in one line.

### End of day

1. Run `facts today`.
2. Write at most 8 lines: done today, still in progress, carry-overs for tomorrow (with ids), blockers.
3. Show it and ask "Save?" On yes (or after their edits), save it with `note --kind report`. Tomorrow's stand-up reads it.

### Weekly review

1. Run `facts week`.
2. At most 15 lines: **Done** (grouped by project), **Slipped** (overdue, and anything that sat in doing all week), **Next week** (top 5 open tasks by due date and priority).
3. Offer to save it as a report. Save only on yes.

### Plan from a document

1. Read the document the user names.
2. Propose tasks as a table: title · project · due (a weekday) · priority. Only what the document actually asks for.
3. Ask "Create these?" and stop. Only after yes, `add` each and list the new ids.
