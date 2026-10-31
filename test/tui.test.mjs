import { test } from 'node:test';
import assert from 'node:assert/strict';
import { STATUSES, addTask, listTasks, openDb, updateTask } from '../skills/scrum/scripts/scrum.mjs';
import { loadBoard, newState, press, render } from '../skills/scrum/scripts/tui.mjs';

const at = (s) => new Date(s).getTime();
const key = (k) => (k.length > 1 ? { name: k } : { sequence: k });
const type = (s, db, keys) => { for (const k of keys) press(s, key(k), db); return s; };
const plain = (screen) => screen.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, '').split('\r\n');

test('board: one column per status, done shows only today, every line fits the width', () => {
  const db = openDb(':memory:');
  const mon = at('2026-10-26T10:00');
  const wed = at('2026-10-28T10:00');
  addTask(db, { title: 'Old win', status: 'done' }, mon);
  addTask(db, { title: 'Fresh win' }, mon);
  updateTask(db, '#2', { status: 'done' }, wed);
  addTask(db, { title: 'Late thing', due: '2026-10-27' }, mon);

  const board = loadBoard(db, wed);
  assert.deepEqual(board[STATUSES.indexOf('done')].map((t) => t.title), ['Fresh win']);
  const lines = plain(render(newState(), board, { width: 90, height: 12, now: wed }));
  assert.equal(lines.length, 12);
  assert.match(lines[0], /1 open · 1 overdue · 0 due today/);
  assert.ok(lines.some((l) => l.includes('#3 Late thing')));
  assert.ok(!lines.some((l) => l.includes('Old win')));
  for (const l of lines) assert.ok([...l].length <= 90, l);
});

test('keys: add, move, block with a reason, unblock, edit, delete', () => {
  const db = openDb(':memory:');
  const s = newState(); // starts on doing
  const task = () => listTasks(db, { status: 'all' })[0];

  type(s, db, ['a', ...'Write docs', 'return']);
  assert.equal(task().status, 'doing');
  type(s, db, ['>']);
  assert.equal(task().status, 'review');
  assert.equal(STATUSES[s.col], 'review'); // the cursor follows the task

  type(s, db, ['b', 'return']);
  assert.match(s.msg, /reason/);
  type(s, db, ['b', ...'no keys', 'return']);
  assert.equal(task().blocked_reason, 'no keys');
  type(s, db, ['right', 'b']);
  assert.equal(task().status, 'doing');

  type(s, db, ['left', 'left', 'e', ...Array(4).fill('backspace'), ...'tests', 'return', 'p']);
  assert.equal(task().title, 'Write tests');
  assert.equal(task().priority, 1);
  assert.match(plain(render(s, loadBoard(db)))[1], /doing 1/);

  type(s, db, ['x', 'n']);
  assert.equal(listTasks(db).length, 1);
  type(s, db, ['x', 'y']);
  assert.equal(listTasks(db).length, 0);

  type(s, db, ['s']);
  assert.ok(plain(render(s, loadBoard(db))).some((l) => l.startsWith('Today so far')));
  type(s, db, ['q']);
  assert.equal(s.facts, null); // any key closes the facts view first
  type(s, db, ['q']);
  assert.ok(s.quit);
});
