# huddle

A personal scrum board and stand-up assistant for [Claude Code](https://claude.com/claude-code), taken from Charlie, my AI co-worker.

Tell Claude what you're working on, and it keeps the board for you. Each morning it writes your stand-up from what actually happened: tasks you finished or moved, and your git commits.

```
> scrum
**Yesterday**
- Shipped the login page and fixed the session timeout bug
**Today**
- Payment webhook (due today)
**Blockers**
- Staging deploy: waiting on AWS keys from ops
```

## What it does

- **Board:** add, move, block (with a reason), reprioritise and finish tasks: `backlog → todo → doing → review → done`, plus `blocked`.
- **Stand-up:** Yesterday / Today / Blockers, ready to paste into Teams or Slack. "Yesterday" is the last day you actually worked, so Monday's stand-up covers Friday.
- **Plan today:** proposes up to 5 tasks (overdue and due first), and moves them to doing only after you say OK.
- **End of day:** an 8-line summary saved as a report; the next stand-up reads it.
- **Weekly review:** what got done, what slipped, and the top 5 for next week.
- **Plan from a document:** turns a spec or brief into proposed tasks, and creates them only after you say yes.
- **Git-aware:** register a repo once, and your commits there show up in stand-ups.

Claude writes the words, but every fact comes from the board, so it can't invent tasks.

## Install

You need Node.js 22.13 or newer. There are no packages to install.

As a plugin, inside Claude Code:

```
/plugin marketplace add tam11a/huddle
/plugin install huddle@huddle
```

Or as a plain skill:

```bash
git clone https://github.com/tam11a/huddle && cp -r huddle/skills/scrum ~/.claude/skills/
```

Then ask Claude things like "add a task to fix the checkout bug, due fri", "I'm blocked on #4, waiting for design", "scrum", "plan my day", "wrap up the day" or "weekly review". As a plugin, it is also `/huddle:scrum`.

## Where the data lives

Everything is in one SQLite file: `~/.local/share/huddle/scrum.db` (or under `$XDG_DATA_HOME`). Nothing leaves your machine.

Set `SCRUM_DB` to use another file, for example one board per job. To set it for Claude Code, add it to `~/.claude/settings.json`:

```json
{ "env": { "SCRUM_DB": "/home/you/.local/share/huddle/work.db" } }
```

The schema is the same as Charlie Lite's, so `SCRUM_DB` can also point at a Charlie database and share its board.

## The board in your terminal

To see the board, or fix it by hand, install the `huddle` command once and run it:

```bash
npm i -g github:tam11a/huddle
huddle
```

It shows one column per status (done shows only today's), with overdue tasks in red and tasks due today in yellow. It uses the same file and rules as Claude, and refreshes every 2 seconds, so you can watch Claude change it.

Keys: arrows or `hjkl` to pick a task, `<` `>` to move it along backlog → todo → doing → review → done, `d` done, `b` block (asks for the reason; on a blocked task, unblocks it), `a` add, `e` edit the title, `t` set the due date, `p` raise the priority, `x` delete, `s` today's facts, `q` quit.

## The script

Claude drives `skills/scrum/scripts/scrum.mjs`, and you can run it yourself too:

```bash
node skills/scrum/scripts/scrum.mjs --help
node skills/scrum/scripts/scrum.mjs add "Write the API docs" --due fri --priority high
node skills/scrum/scripts/scrum.mjs facts today
```

Tests: `npm test`.

## License

MIT
