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
- **Reports with charts:** "daily report" or "how did my week go" gets you tasks done per day, your workday streak, cycle time (doing → done), hours blocked, per-project counts, what's sitting still and what carries over. In the Claude app it's a page with real charts; in the terminal, press `r` on the board.
- **Plan from a document:** turns a spec or brief into proposed tasks, and creates them only after you say yes.
- **Git-aware:** register a repo once (`huddle project add <slug>`), and your commits there show up in stand-ups. It also adds a post-commit hook: a commit that names `#12` starts that task, and `fixes #12` finishes it. Only tasks of that repo's project move, so GitHub issue numbers elsewhere are safe.
- **Keeps Claude up to date:** when a Claude Code session starts, Claude sees what's in doing, blocked, in review and overdue, and whether the folder is a huddle project.

Claude writes the words, but every fact comes from the board, so it can't invent tasks.

## Install

First install the `huddle` command, one small binary with SQLite built in. You need [Rust](https://rustup.rs) to build it:

```bash
cargo install --git https://github.com/tam11a/huddle
```

While the repo is private, cargo needs your git login: run it with `CARGO_NET_GIT_FETCH_WITH_CLI=true` in front.

Then add the plugin inside Claude Code. It brings the `scrum` skill and connects Claude to the board through `huddle mcp`, an MCP server with tools like `add_task`, `move_task`, `list_tasks` and `facts`:

```
/plugin marketplace add tam11a/huddle
/plugin install huddle@huddle
```

Claude then keeps the board true as it works: when it starts, finishes or gets blocked on a task that's on the board, it moves the task and says so.

To use the board from another MCP client (Claude Desktop, or Claude Code without the plugin), point it at `huddle mcp`:

```bash
claude mcp add -s user huddle -- huddle mcp
```

```json
{ "mcpServers": { "huddle": { "command": "huddle", "args": ["mcp"] } } }
```

Or add just the skill, which runs `huddle` commands instead of the tools:

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

To see the board, or fix it by hand, run `huddle` with no arguments. It's a dashboard in the style of btop:

- a header with today's counts and a progress bar for what you've finished today
- one coloured column per status, with a card for each task: priority dot, due date (red when overdue, yellow when due today), project and blocker
- the selected task's details, a chart of tasks done over the last 14 days, and open and done counts per project (when the terminal is at least 90×24)
- a stand-up view (`s`) with Yesterday, Today and Blockers, and a help screen (`?`)

It moves a little: a short intro, a flash on a card when it changes, notifications that slide in and fade, and a progress bar and chart that fill up. It only redraws while something is moving, so it uses almost no CPU when idle. It uses the same file and rules as Claude and refreshes every 2 seconds, so you can watch Claude change the board. It uses 24-bit colour; set `NO_COLOR=1` to turn colours off.

Keys: arrows or `hjkl` to pick a task, `<` `>` to move it along backlog → todo → doing → review → done, `d` done, `b` block (asks for the reason; on a blocked task, unblocks it), `a` add, `e` edit the title, `t` set the due date, `p` raise the priority, `x` delete, `s` stand-up, `?` help, `q` quit.

## The commands

Claude drives `huddle` through commands that print JSON, and you can run them yourself too:

```bash
huddle --help
huddle add "Write the API docs" --due fri --priority high
huddle facts today
```

Build and test from a clone: `cargo build --release`, `cargo test`.

## License

MIT
