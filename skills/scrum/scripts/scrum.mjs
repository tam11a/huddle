#!/usr/bin/env node
// Scrum board for Claude: tasks, notes and stand-up facts in one SQLite file. Zero dependencies (Node >= 22.13).
// Ported from Charlie Lite; the schema is identical, so SCRUM_DB can point at a Charlie database.
import { DatabaseSync } from 'node:sqlite';
import { execFileSync } from 'node:child_process';
import { mkdirSync, realpathSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

export const STATUSES = ['backlog', 'todo', 'doing', 'review', 'blocked', 'done'];
export const NOTE_KINDS = ['note', 'decision', 'report'];
export const PERIODS = ['last_workday', 'today', 'week'];
const PRIORITY_WORDS = { urgent: 0, high: 1, normal: 2, medium: 2, low: 3 };
const DAY = 864e5;
const LOOKBACK_DAYS = 14;

// ---------- db ----------

export const dbFile = () =>
  process.env.SCRUM_DB ?? join(process.env.XDG_DATA_HOME ?? join(homedir(), '.local/share'), 'huddle/scrum.db');

// Times are epoch milliseconds. Append-only: never edit a shipped migration, add a new one.
// Migration 1 is Charlie Lite's, unchanged (reminders included), so the two can share a file.
const MIGRATIONS = [
  `
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
  `,
];

export function openDb(file = dbFile()) {
  if (file !== ':memory:') mkdirSync(dirname(file), { recursive: true });
  const db = new DatabaseSync(file);
  db.exec('PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;');
  const { user_version } = db.prepare('PRAGMA user_version').get();
  for (let v = user_version; v < MIGRATIONS.length; v++) {
    tx(db, () => {
      db.exec(MIGRATIONS[v]);
      db.exec(`PRAGMA user_version = ${v + 1}`);
    });
  }
  return db;
}

function tx(db, fn) {
  db.exec('BEGIN IMMEDIATE');
  try {
    const out = fn();
    db.exec('COMMIT');
    return out;
  } catch (e) {
    db.exec('ROLLBACK');
    throw e;
  }
}

// ---------- time ----------

export function startOfDay(t) {
  const d = new Date(t);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** ISO ("2026-10-31", "2026-10-31T09:30"), "+30m" / "in 2h" / "+3d", "today", "tomorrow", weekday names, each with an optional time ("9:30", "5pm"). */
export function parseWhen(input, now = new Date()) {
  const s = input.trim().toLowerCase();
  const rel = s.match(/^(?:in\s+|\+)(\d+)\s*(m|mins?|minutes?|h|hrs?|hours?|d|days?)$/);
  if (rel) return now.getTime() + Number(rel[1]) * { m: 6e4, h: 36e5, d: DAY }[rel[2][0]];

  const m = s.match(/^(today|tomorrow|tmrw|sun\w*|mon\w*|tue\w*|wed\w*|thu\w*|fri\w*|sat\w*|\d{4}-\d{2}-\d{2})?(?:(?:^|\s+|t)(?:at\s+)?(\d{1,2})(?::(\d{2}))?\s*(am|pm)?)?$/);
  if (m && (m[1] || m[2])) {
    const [, day, hh, mm, ampm] = m;
    let base = startOfDay(now);
    if (day === 'tomorrow' || day === 'tmrw') base += DAY;
    else if (day && /^\d/.test(day)) {
      const [y, mo, d] = day.split('-').map(Number);
      base = new Date(y, mo - 1, d).getTime();
    } else if (day && day !== 'today') {
      const want = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'].indexOf(day.slice(0, 3));
      if (want < 0) throw new Error(`can't read the day in "${input}"`);
      base += ((want - now.getDay() + 7) % 7) * DAY;
    }
    let hour = hh === undefined ? 18 : Number(hh); // a date with no time means "by end of the workday"
    if (ampm === 'pm' && hour < 12) hour += 12;
    if (ampm === 'am' && hour === 12) hour = 0;
    if (hour > 23 || Number(mm ?? 0) > 59) throw new Error(`can't read the time in "${input}"`);
    const d = new Date(base);
    d.setHours(hour, mm ? Number(mm) : 0, 0, 0);
    if (!day && d.getTime() < now.getTime()) d.setDate(d.getDate() + 1);
    return d.getTime();
  }

  const t = new Date(input).getTime();
  if (Number.isNaN(t)) throw new Error(`can't read the date/time "${input}" (try 2026-10-31, tomorrow 9:30, fri, +2h)`);
  return t;
}

export const fmtWhen = (t) =>
  new Date(t).toLocaleString('en-GB', { weekday: 'short', day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' });
export const fmtDay = (t) => new Date(t).toLocaleDateString('en-GB', { weekday: 'short', day: 'numeric', month: 'short' });

// ---------- tasks ----------

export const fmtId = (id) => `#${id}`;

/** "#42", "42", or a Charlie id like "O-42" → 42. */
export function parseId(ref) {
  const m = String(ref).trim().match(/^(?:[a-z]-?)?#?(\d+)$/i);
  if (!m) throw new Error(`"${ref}" is not a task id (looks like #12)`);
  return Number(m[1]);
}

export function parsePriority(p) {
  if (p === undefined || p === '') return undefined;
  const n = typeof p === 'number' ? p : PRIORITY_WORDS[p.toLowerCase()] ?? Number(p);
  if (!Number.isInteger(n) || n < 0 || n > 3) throw new Error(`priority must be 0-3 or urgent/high/normal/low, got "${p}"`);
  return n;
}

function checkStatus(s) {
  if (!STATUSES.includes(s)) throw new Error(`status must be one of ${STATUSES.join(', ')}; got "${s}"`);
  return s;
}

function logEvent(db, type, taskId, payload = {}, now = Date.now()) {
  db.prepare('INSERT INTO events (type, source, task_id, payload, created_at) VALUES (?, ?, ?, ?, ?)')
    .run(type, 'scrum', taskId, JSON.stringify(payload), now);
}

export function addProject(db, slug, path) {
  slug = slug.trim().toLowerCase();
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(slug)) throw new Error(`project slug "${slug}" may only use a-z, 0-9, . _ -`);
  return tx(db, () => {
    db.prepare('INSERT INTO projects (slug, path) VALUES (?, ?) ON CONFLICT(slug) DO UPDATE SET path = excluded.path').run(slug, path);
    logEvent(db, 'project.add', null, { slug, path });
    return db.prepare('SELECT slug, path FROM projects WHERE slug = ?').get(slug);
  });
}

export const listProjects = (db) => db.prepare('SELECT slug, path FROM projects ORDER BY slug').all();

function projectId(db, slug) {
  if (!slug) return null;
  const row = db.prepare('SELECT id FROM projects WHERE slug = ?').get(slug.trim().toLowerCase());
  if (row) return row.id;
  const known = listProjects(db).map((p) => p.slug);
  throw new Error(`unknown project "${slug}" (known: ${known.join(', ') || 'none yet; add one with `project add`'})`);
}

export function view(r) {
  const v = { id: fmtId(r.id), title: r.title, status: r.status, priority: r.priority };
  if (r.project) v.project = r.project;
  if (r.due_at) v.due = fmtWhen(r.due_at);
  if (r.blocked_reason) v.blocked_reason = r.blocked_reason;
  if (r.description) v.description = r.description;
  return v;
}

const TASK_SELECT = 'SELECT t.*, p.slug AS project FROM tasks t LEFT JOIN projects p ON p.id = t.project_id';

function getRow(db, id) {
  const r = db.prepare(`${TASK_SELECT} WHERE t.id = ?`).get(id);
  if (!r) throw new Error(`no task ${fmtId(id)}`);
  return r;
}

export function addTask(db, input, now = Date.now()) {
  const title = input.title?.trim();
  if (!title) throw new Error('a task needs a title');
  const status = checkStatus(input.status ?? 'todo');
  const priority = parsePriority(input.priority) ?? 2;
  const due = input.due ? parseWhen(input.due, new Date(now)) : null;
  return tx(db, () => {
    const { lastInsertRowid } = db.prepare(
      `INSERT INTO tasks (project_id, title, description, status, priority, due_at, created_at, updated_at, done_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    ).run(projectId(db, input.project), title, input.description ?? '', status, priority, due, now, now, status === 'done' ? now : null);
    const id = Number(lastInsertRowid);
    logEvent(db, 'task.add', id, { title, status }, now);
    return view(getRow(db, id));
  });
}

/** patch: status, title, due ("none" clears), priority, project, blocked_reason, description, delete, restore. */
export function updateTask(db, ref, patch, now = Date.now()) {
  const id = parseId(ref);
  return tx(db, () => {
    const cur = getRow(db, id);
    if (cur.deleted_at && !patch.restore) throw new Error(`${fmtId(id)} is deleted; restore it first`);
    if (patch.status === 'blocked' && !patch.blocked_reason && !cur.blocked_reason) throw new Error('blocking a task needs a reason');
    const set = {};
    const changes = {};

    if (patch.title?.trim()) set.title = patch.title.trim();
    if (patch.description !== undefined) set.description = patch.description;
    if (patch.priority !== undefined && patch.priority !== '') set.priority = parsePriority(patch.priority);
    if (patch.due !== undefined) set.due_at = patch.due && patch.due !== 'none' ? parseWhen(patch.due, new Date(now)) : null;
    if (patch.project !== undefined) set.project_id = projectId(db, patch.project);
    if (patch.blocked_reason) set.blocked_reason = patch.blocked_reason;
    if (patch.status) {
      const status = checkStatus(patch.status);
      if (status !== cur.status) {
        set.status = status;
        changes.from = cur.status;
        changes.to = status;
        set.done_at = status === 'done' ? now : null;
        if (status !== 'blocked') set.blocked_reason = null;
      }
    }
    if (patch.delete) set.deleted_at = now;
    if (patch.restore) set.deleted_at = null;

    const keys = Object.keys(set);
    if (!keys.length) return view(cur);
    db.prepare(`UPDATE tasks SET ${keys.map((k) => `${k} = ?`).join(', ')}, updated_at = ? WHERE id = ?`)
      .run(...keys.map((k) => set[k]), now, id);

    const type = patch.delete ? 'task.delete' : patch.restore ? 'task.restore' : changes.to ? 'task.status' : 'task.update';
    logEvent(db, type, id, { ...changes, fields: keys }, now);
    return view(getRow(db, id));
  });
}

/** status: a status name, "open" (default: everything not done), or "all". Deleted tasks never show. */
export function listTasks(db, f = {}, now = Date.now()) {
  const where = ['t.deleted_at IS NULL'];
  const args = [];
  const status = f.status || 'open';
  if (status === 'open') where.push("t.status != 'done'");
  else if (status !== 'all') { where.push('t.status = ?'); args.push(checkStatus(status)); }
  if (f.project) { where.push('t.project_id = ?'); args.push(projectId(db, f.project)); }
  if (f.query && /^(?:[a-z]-?)?#?\d+$/i.test(f.query.trim())) { where.push('t.id = ?'); args.push(parseId(f.query)); }
  // shortcut: LIKE scan, move to FTS5 if tasks pass ~5k rows
  else if (f.query) { where.push('(t.title LIKE ? OR t.description LIKE ?)'); args.push(`%${f.query}%`, `%${f.query}%`); }
  if (f.due_before) { where.push('t.due_at IS NOT NULL AND t.due_at < ?'); args.push(parseWhen(f.due_before, new Date(now))); }
  return db.prepare(
    `${TASK_SELECT} WHERE ${where.join(' AND ')}
     ORDER BY CASE t.status WHEN 'doing' THEN 0 WHEN 'blocked' THEN 1 WHEN 'review' THEN 2 WHEN 'todo' THEN 3 WHEN 'backlog' THEN 4 ELSE 5 END,
              t.priority, t.due_at IS NULL, t.due_at, t.id
     LIMIT ?`,
  ).all(...args, f.limit ?? 200).map((r) => ({ ...view(r), due_at: r.due_at ?? undefined }));
}

export function addNote(db, n, now = Date.now()) {
  const content = n.content?.trim();
  if (!content) throw new Error('a note needs content');
  const kind = n.kind || 'note';
  if (!NOTE_KINDS.includes(kind)) throw new Error(`note kind must be one of ${NOTE_KINDS.join(', ')}`);
  return tx(db, () => {
    const taskId = n.task ? parseId(n.task) : null;
    if (taskId) getRow(db, taskId);
    const { lastInsertRowid } = db.prepare('INSERT INTO notes (project_id, task_id, kind, content, created_at) VALUES (?, ?, ?, ?, ?)')
      .run(projectId(db, n.project), taskId, kind, content, now);
    logEvent(db, 'note.add', taskId, { kind }, now);
    return { id: Number(lastInsertRowid), kind, saved: fmtWhen(now) };
  });
}

// ---------- facts ----------

/** Your commits in every registered project, using each repo's own user.email. */
export function commits(db, from, to) {
  const out = [];
  for (const p of listProjects(db)) {
    try {
      const git = (...args) => execFileSync('git', ['-C', p.path, ...args], { encoding: 'utf8', timeout: 5000, stdio: ['ignore', 'pipe', 'ignore'] });
      const email = git('config', 'user.email').trim();
      const log = git('log', '--all', '--no-merges', `--author=${email}`, `--since=${new Date(from).toISOString()}`,
        `--until=${new Date(to).toISOString()}`, '--format=%h%x09%ct%x09%s');
      for (const line of log.split('\n').filter(Boolean)) {
        const [hash, ct, ...subject] = line.split('\t');
        out.push({ project: p.slug, hash, subject: subject.join('\t'), at: Number(ct) * 1000 });
      }
    } catch {
      // project path gone, not a repo, or no email set: skip it rather than fail the stand-up
    }
  }
  return out.sort((a, b) => a.at - b.at);
}

/** The most recent day before today with any board activity or commits; yesterday if there's no history. */
export function lastWorkday(db, now = Date.now()) {
  const today = startOfDay(now);
  const oldest = startOfDay(today - LOOKBACK_DAYS * DAY);
  const times = [
    ...commits(db, oldest, today).map((c) => c.at),
    ...db.prepare('SELECT created_at FROM events WHERE created_at >= ? AND created_at < ?').all(oldest, today).map((r) => r.created_at),
  ];
  const latest = Math.max(-Infinity, ...times);
  const from = Number.isFinite(latest) ? startOfDay(latest) : startOfDay(today - 1);
  return { from, to: startOfDay(from + 36 * 36e5) }; // +36h then floor = next midnight, safe across DST
}

const brief = ({ id, title, status, project, due, priority, blocked_reason }) =>
  ({ id, title, status, ...(project && { project }), ...(due && { due }), ...(priority !== 2 && { priority }), ...(blocked_reason && { blocked_reason }) });

/** Everything a stand-up, plan, end-of-day or weekly review needs, as one JSON object. */
export function facts(db, period, now = Date.now()) {
  if (!PERIODS.includes(period)) throw new Error(`period must be one of ${PERIODS.join(', ')}`);
  const today = startOfDay(now);
  const tomorrow = startOfDay(today + 36 * 36e5);
  const { from, to } =
    period === 'today' ? { from: today, to: now }
    : period === 'week' ? { from: startOfDay(today - 6 * DAY), to: now }
    : lastWorkday(db, now);

  const open = listTasks(db, { status: 'open' }, now);
  const rows = (sql, ...args) => db.prepare(sql).all(...args);
  const live = `${TASK_SELECT} WHERE t.deleted_at IS NULL`;

  // last status change per task in the window, ignoring moves to done (those are in `done`)
  const moved = new Map();
  for (const e of rows("SELECT e.task_id, e.payload, t.title FROM events e JOIN tasks t ON t.id = e.task_id WHERE e.type = 'task.status' AND e.created_at >= ? AND e.created_at < ? AND t.deleted_at IS NULL ORDER BY e.created_at", from, to)) {
    const p = JSON.parse(e.payload);
    moved.set(e.task_id, { id: fmtId(e.task_id), title: e.title, from: moved.get(e.task_id)?.from ?? p.from, to: p.to });
  }
  const lastReport = rows("SELECT content, created_at FROM notes WHERE kind = 'report' AND created_at < ? ORDER BY created_at DESC LIMIT 1", period === 'week' ? now : today)[0];

  return {
    period,
    window: { from: new Date(from).toISOString(), to: new Date(to).toISOString(), label: period === 'week' ? `${fmtDay(from)} – ${fmtDay(now)}` : fmtDay(from) },
    now: fmtWhen(now),
    done: rows(`${live} AND t.done_at >= ? AND t.done_at < ? ORDER BY t.done_at`, from, to).map((r) => brief(view(r))),
    added: rows(`${live} AND t.created_at >= ? AND t.created_at < ?`, from, to).map((r) => brief(view(r))),
    moved: [...moved.values()].filter((m) => m.to !== 'done' && m.from !== m.to),
    doing: open.filter((t) => t.status === 'doing').map(brief),
    review: open.filter((t) => t.status === 'review').map(brief),
    blocked: open.filter((t) => t.status === 'blocked').map(brief),
    due_today: open.filter((t) => t.due_at >= today && t.due_at < tomorrow).map(brief),
    overdue: open.filter((t) => t.due_at < today).map(brief),
    todo_count: open.filter((t) => t.status === 'todo').length,
    decisions: rows("SELECT content FROM notes WHERE kind = 'decision' AND created_at >= ? AND created_at < ? ORDER BY created_at", from, to).map((n) => n.content),
    commits: commits(db, from, to).map(({ project, hash, subject }) => ({ project, hash, subject })),
    last_report: lastReport ? { written: fmtWhen(lastReport.created_at), text: lastReport.content } : null,
  };
}

// ---------- cli ----------

const USAGE = `scrum.mjs <command> [args]   (prints JSON; board file: ${dbFile()})
  add "<title>" [--project p] [--due d] [--priority urgent|high|normal|low] [--status s] [--desc text]
  move <id> <status> [--reason text]          statuses: ${STATUSES.join(', ')} (blocked needs --reason)
  update <id> [--title t] [--due d|none] [--priority p] [--project p] [--desc text] [--delete] [--restore]
  list [--status s|open|all] [--project p] [--query words] [--due-before d]
  note "<text>" [--kind note|decision|report] [--task id] [--project p]
  project add <slug> [path]    (path defaults to the current folder; its git commits feed the stand-up)
  project list
  facts last_workday|today|week
dates: 2026-10-31, 2026-10-31T09:30, today, tomorrow 9:30, fri, +2h`;

export function run(argv, db = openDb()) {
  const { values: o, positionals: [cmd, ...p] } = parseArgs({
    args: argv,
    allowPositionals: true,
    options: Object.fromEntries(
      ['project', 'due', 'priority', 'status', 'desc', 'reason', 'title', 'query', 'due-before', 'kind', 'task']
        .map((k) => [k, { type: 'string' }])
        .concat([['delete', { type: 'boolean' }], ['restore', { type: 'boolean' }], ['help', { type: 'boolean', short: 'h' }]]),
    ),
  });
  const need = (v, what) => { if (!v) throw new Error(`missing ${what}\n\n${USAGE}`); return v; };
  switch (o.help ? 'help' : cmd) {
    case 'add': return addTask(db, { title: need(p.join(' '), 'title'), project: o.project, due: o.due, priority: o.priority, status: o.status, description: o.desc });
    case 'move': return updateTask(db, need(p[0], 'task id'), { status: need(p[1], 'status'), blocked_reason: o.reason });
    case 'update': return updateTask(db, need(p[0], 'task id'), {
      title: o.title, due: o.due, priority: o.priority, project: o.project, status: o.status,
      blocked_reason: o.reason, description: o.desc, delete: o.delete, restore: o.restore,
    });
    case 'list': return listTasks(db, { status: o.status, project: o.project, query: o.query, due_before: o['due-before'], limit: 50 })
      .map(({ due_at, ...t }) => t);
    case 'note': return addNote(db, { content: need(p.join(' '), 'note text'), kind: o.kind, task: o.task, project: o.project });
    case 'project':
      if (p[0] === 'add') return addProject(db, need(p[1], 'project slug'), resolve(p[2] ?? '.'));
      if (p[0] === 'list') return listProjects(db);
      throw new Error(`project add|list\n\n${USAGE}`);
    case 'facts': return facts(db, need(p[0], 'period'));
    case 'help': case undefined: return USAGE;
    default: throw new Error(`unknown command "${cmd}"\n\n${USAGE}`);
  }
}

if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const out = run(process.argv.slice(2));
    console.log(typeof out === 'string' ? out : JSON.stringify(out));
  } catch (e) {
    console.error(`Error: ${e.message}`);
    process.exit(1);
  }
}
