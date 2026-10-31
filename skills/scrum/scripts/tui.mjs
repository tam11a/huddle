#!/usr/bin/env node
// huddle: a terminal board for the scrum skill. Same SQLite file and rules as scrum.mjs, so stand-ups see every move made here.
import { realpathSync } from 'node:fs';
import { emitKeypressEvents } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { STATUSES, addTask, dbFile, facts, listTasks, openDb, startOfDay, updateTask } from './scrum.mjs';

const FLOW = ['backlog', 'todo', 'doing', 'review', 'done']; // < and > step along this; blocked sits beside doing
const PRIORITY = ['urgent', 'high', 'normal', 'low'];
const KEYS = '←→↑↓ pick  < > move  d done  b block  a add  e title  t due  p priority  x delete  s today  q quit';
const ESC = '\x1b[';
const paint = (code, s) => (code ? `${ESC}${code}m${s}${ESC}0m` : s);

// shortcut: counts code points, so wide CJK/emoji titles can push a row past the edge; use a wcwidth table if that bites
function fit(str, w) {
  if (w <= 0) return '';
  const c = [...String(str ?? '').replace(/[\r\n\t]+/g, ' ')];
  return c.length > w ? c.slice(0, w - 1).join('') + '…' : c.join('') + ' '.repeat(w - c.length);
}

/** One array of tasks per status, in STATUSES order; done holds only what was done today. */
export function loadBoard(db, now = Date.now()) {
  // shortcut: shows at most 1000 tasks, page the columns if a board ever grows past that
  const tasks = listTasks(db, { status: 'all', done_since: startOfDay(now), limit: 1000 }, now);
  return STATUSES.map((s) => tasks.filter((t) => t.status === s));
}

export const newState = () => ({ col: STATUSES.indexOf('doing'), rows: STATUSES.map(() => 0), prompt: null, facts: null, msg: '', err: false, quit: false });

/** Clamps the cursor to the board and returns the selected task, if any. */
function pick(s, board) {
  s.rows = s.rows.map((r, i) => Math.max(0, Math.min(r, board[i].length - 1)));
  return board[s.col][s.rows[s.col]];
}

function follow(s, db, id, now) {
  const board = loadBoard(db, now);
  const col = board.findIndex((c) => c.some((t) => t.id === id));
  if (col >= 0) { s.col = col; s.rows[col] = board[col].findIndex((t) => t.id === id); }
}

/** Applies one keypress (a readline key object) to the state and the board. Errors become the status message. */
export function press(s, key, db, now = Date.now()) {
  const k = key.name?.length > 1 && key.name !== 'space' ? key.name : key.sequence;
  if (k === '\x03') return Object.assign(s, { quit: true });
  Object.assign(s, { msg: '', err: false });
  try {
    if (s.prompt) typeInto(s, k);
    else if (s.facts) s.facts = null;
    else boardKey(s, k, db, now);
  } catch (e) {
    Object.assign(s, { msg: e.message, err: true, prompt: null });
  }
  return s;
}

function typeInto(s, k) {
  const p = s.prompt;
  if (p.confirm) { s.prompt = null; if (k === 'y') p.submit(); }
  else if (k === 'escape') s.prompt = null;
  else if (k === 'return' || k === 'enter') { s.prompt = null; p.submit(p.value.trim()); }
  else if (k === 'backspace') p.value = [...p.value].slice(0, -1).join('');
  else if ([...(k ?? '')].length === 1 && k >= ' ') p.value += k;
}

function boardKey(s, k, db, now) {
  const t = pick(s, loadBoard(db, now));
  const ask = (label, submit, value = '') => { s.prompt = { label, value, submit }; };
  const change = (patch, what = 'updated', go = false) => {
    const r = updateTask(db, t.id, patch, now);
    if (go) follow(s, db, r.id, now);
    s.msg = `${r.id} ${r.title}: ${what}`;
  };
  switch (k) {
    case 'left': case 'h': s.col = Math.max(0, s.col - 1); return;
    case 'right': case 'l': s.col = Math.min(STATUSES.length - 1, s.col + 1); return;
    case 'up': case 'k': s.rows[s.col]--; return;
    case 'down': case 'j': s.rows[s.col]++; return;
    case 'q': s.quit = true; return;
    case 's': s.facts = facts(db, 'today', now); return;
    case 'a': return ask('New task', (title) => {
      const here = STATUSES[s.col];
      const r = addTask(db, { title, status: here === 'blocked' || here === 'done' ? 'todo' : here }, now);
      follow(s, db, r.id, now);
      s.msg = `${r.id} ${r.title}: added`;
    });
  }
  if (!t) return; // the rest act on the selected task
  const i = FLOW.indexOf(t.status === 'blocked' ? 'doing' : t.status);
  switch (k) {
    case '<': if (i > 0) change({ status: FLOW[i - 1] }, `→ ${FLOW[i - 1]}`, true); return;
    case '>': if (i < FLOW.length - 1) change({ status: FLOW[i + 1] }, `→ ${FLOW[i + 1]}`, true); return;
    case 'd': return change({ status: 'done' }, '→ done');
    case 'b': return t.status === 'blocked'
      ? change({ status: 'doing' }, '→ doing')
      : ask('Blocked on', (reason) => change({ status: 'blocked', blocked_reason: reason }, '→ blocked'));
    case 'e': return ask('Title', (title) => change({ title }), t.title);
    case 't': return ask('Due (fri, tomorrow 9:30, none)', (due) => due && change({ due }));
    case 'p': return change({ priority: (t.priority + 3) % 4 }, `priority ${PRIORITY[(t.priority + 3) % 4]}`);
    case 'x': s.prompt = { label: `Delete ${t.id} ${t.title}? y/n`, value: '', confirm: true, submit: () => change({ delete: true }, 'deleted') };
  }
}

function factsLines(f) {
  const list = (title, items, show = (t) => `${t.id} ${t.title}`) =>
    [[title, '1'], ...(items.length ? items.map((x) => [`  ${show(x)}`]) : [['  none', '2']])];
  return [
    [`Today so far (${f.window.label})`, '1;4'],
    ...list('Done', f.done), ...list('Doing', f.doing), ...list('Due today', f.due_today), ...list('Overdue', f.overdue),
    ...list('Blocked', f.blocked, (t) => `${t.id} ${t.title}: ${t.blocked_reason}`),
    ...list('Commits', f.commits, (c) => `${c.project} ${c.hash} ${c.subject}`),
    ['any key goes back', '2'],
  ];
}

/** The whole screen as one string of ANSI: header, one column per status, the selected task, a message, then keys or a prompt. */
export function render(s, board, { width = 100, height = 30, now = Date.now(), file = '' } = {}) {
  const line = (text, code) => paint(code, fit(text, width));
  const today = startOfDay(now);
  const late = (t) => t.due_at < today;
  const dueToday = (t) => t.due_at >= today && t.due_at < startOfDay(today + 36 * 36e5);
  const open = board.flat().filter((t) => t.status !== 'done');
  const out = [line(['huddle', `${open.length} open`, `${open.filter(late).length} overdue`, `${open.filter(dueToday).length} due today`, file].filter(Boolean).join(' · '), '1')];

  if (s.facts) out.push(...factsLines(s.facts).map(([text, code]) => line(text, code)));
  else {
    const w = Math.floor(width / STATUSES.length);
    const t = pick(s, board);
    const body = Math.max(1, height - 5);
    const top = Math.max(0, s.rows[s.col] - body + 1); // only the selected column scrolls
    out.push(STATUSES.map((st, i) => paint(i === s.col ? '1;4' : '2', fit(` ${st} ${board[i].length}`, w))).join(''));
    for (let r = 0; r < body; r++) {
      out.push(board.map((col, i) => {
        const x = col[r + (i === s.col ? top : 0)];
        if (!x) return ' '.repeat(w);
        const code = x === t ? '7' : x.status === 'done' ? '2' : late(x) ? '31' : dueToday(x) ? '33' : x.priority === 3 ? '2' : '';
        return paint(code, fit(` ${'!'.repeat(Math.max(0, 2 - x.priority))}${x.id} ${x.title}`, w));
      }).join(''));
    }
    out.push(line(t
      ? [`${t.id} ${t.title}`, t.project, t.due && `due ${t.due}`, PRIORITY[t.priority], t.blocked_reason && `blocked: ${t.blocked_reason}`, t.description].filter(Boolean).join(' · ')
      : 'empty column; a adds a task'));
    out.push(line(s.msg, s.err ? '31' : '32'));
    out.push(s.prompt ? line(`${s.prompt.label}: ${s.prompt.value}▏`, '1') : line(KEYS, '2'));
  }
  return `${ESC}H${out.slice(0, height).map((l) => l + `${ESC}K`).join('\r\n')}${ESC}J`;
}

function main() {
  const { stdin, stdout } = process;
  if (!stdin.isTTY || !stdout.isTTY) throw new Error('huddle needs a terminal; scripts can use scrum.mjs');
  const db = openDb();
  const s = newState();
  const draw = () => stdout.write(render(s, loadBoard(db), { width: stdout.columns, height: stdout.rows, file: dbFile() }));
  const timer = setInterval(draw, 2000); // picks up what Claude changes while the board is open
  const quit = () => {
    clearInterval(timer);
    stdout.off('resize', draw);
    stdout.write(`${ESC}?25h${ESC}?1049l`);
    stdin.setRawMode(false);
    stdin.pause();
    db.close();
  };
  emitKeypressEvents(stdin);
  stdin.setRawMode(true);
  stdout.write(`${ESC}?1049h${ESC}?25l`);
  stdin.on('keypress', (seq, key) => {
    press(s, key ?? { sequence: seq }, db);
    s.quit ? quit() : draw();
  });
  stdout.on('resize', draw);
  process.on('uncaughtException', (e) => { quit(); console.error(`Error: ${e.message}`); process.exitCode = 1; }); // leave a usable terminal
  draw();
}

if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (e) {
    console.error(`Error: ${e.message}`);
    process.exit(1);
  }
}
