import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { addNote, addTask, facts, listTasks, openDb, parseId, parseWhen, run, startOfDay, updateTask } from '../skills/scrum/scripts/scrum.mjs';

const at = (s) => new Date(s).getTime();

test('parseWhen reads the short forms', () => {
  const now = new Date('2026-10-28T10:00'); // a Wednesday
  assert.equal(parseWhen('fri', now), at('2026-10-30T18:00'));
  assert.equal(parseWhen('tomorrow 9:30', now), at('2026-10-29T09:30'));
  assert.equal(parseWhen('+2h', now), at('2026-10-28T12:00'));
  assert.equal(parseWhen('9am', now), at('2026-10-29T09:00')); // already past today, so tomorrow
  assert.throws(() => parseWhen('someday', now));
});

test('parseId accepts #12, 12 and Charlie ids', () => {
  assert.equal(parseId('#12'), 12);
  assert.equal(parseId('12'), 12);
  assert.equal(parseId('O-12'), 12);
  assert.throws(() => parseId('twelve'));
});

test('tasks move through the board and blocking needs a reason', () => {
  const db = openDb(':memory:');
  const t = addTask(db, { title: 'Write the API', priority: 'high', due: '2026-11-02' });
  assert.equal(t.id, '#1');
  assert.throws(() => updateTask(db, t.id, { status: 'blocked' }), /reason/);
  assert.equal(updateTask(db, '1', { status: 'blocked', blocked_reason: 'waiting on keys' }).blocked_reason, 'waiting on keys');
  assert.equal(updateTask(db, '#1', { status: 'doing' }).blocked_reason, undefined);
  updateTask(db, '#1', { status: 'done' });
  assert.equal(listTasks(db).length, 0);
  assert.equal(listTasks(db, { status: 'all' }).length, 1);
  updateTask(db, '#1', { delete: true });
  assert.equal(listTasks(db, { status: 'all' }).length, 0);
});

test('facts: yesterday is the last day with activity, today has doing / due / overdue / blocked', () => {
  const db = openDb(':memory:');
  const mon = at('2026-10-26T10:00');
  const wed = at('2026-10-28T09:00');
  addTask(db, { title: 'Ship login', status: 'doing' }, mon);
  updateTask(db, '#1', { status: 'done' }, mon + 36e5);
  addTask(db, { title: 'Fix crash', status: 'doing' }, mon);
  addTask(db, { title: 'Old thing', due: '2026-10-27' }, mon);
  addTask(db, { title: 'Due today', due: 'today' }, wed);
  addTask(db, { title: 'Stuck', status: 'todo' }, mon);
  updateTask(db, '#5', { status: 'blocked', blocked_reason: 'no access' }, mon);
  addNote(db, { content: 'Mon: login shipped', kind: 'report' }, mon + 2 * 36e5);

  const y = facts(db, 'last_workday', wed);
  assert.equal(y.window.from, new Date(startOfDay(mon)).toISOString()); // Tuesday was quiet, so Monday
  assert.deepEqual(y.done.map((t) => t.title), ['Ship login']);

  const t = facts(db, 'today', wed);
  assert.deepEqual(t.doing.map((x) => x.title), ['Fix crash']);
  assert.deepEqual(t.overdue.map((x) => x.title), ['Old thing']);
  assert.deepEqual(t.due_today.map((x) => x.title), ['Due today']);
  assert.deepEqual(t.blocked.map((x) => x.blocked_reason), ['no access']);
  assert.equal(t.last_report.text, 'Mon: login shipped');
});

test('cli: run() parses commands and the script prints JSON', () => {
  const db = openDb(':memory:');
  assert.equal(run(['add', 'Write', 'docs', '--priority', 'low'], db).title, 'Write docs');
  assert.equal(run(['move', '#1', 'review'], db).status, 'review');
  assert.equal(run(['list', '--status', 'review'], db).length, 1);
  assert.throws(() => run(['move', '#1'], db), /missing status/);
  const out = execFileSync(process.execPath, ['skills/scrum/scripts/scrum.mjs', 'list'], { env: { ...process.env, SCRUM_DB: ':memory:' }, encoding: 'utf8' });
  assert.deepEqual(JSON.parse(out), []);
});
