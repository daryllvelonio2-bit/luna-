import assert from 'node:assert/strict';
import {
  parseScheduleRule,
  formatScheduleRule,
  describeScheduleRule,
  formatTime12h,
} from '../src/scheduleUtils.ts';

console.log('Running scheduleUtils unit tests...');

// 1. Daily local rule parsing & formatting
{
  const parsed = parseScheduleRule('daily-local:20:00');
  assert.equal(parsed.kind, 'daily');
  assert.equal(parsed.isLocal, true);
  assert.equal(parsed.dailyTime, '20:00');

  const desc = describeScheduleRule('daily-local:20:00');
  assert.equal(desc.summary, 'Daily at 8:00 PM');
  assert.equal(desc.isLocal, true);
  assert.match(desc.detail, /local/i);

  const formatted = formatScheduleRule({
    kind: 'daily',
    isLocal: true,
    dailyTime: '20:00',
  });
  assert.equal(formatted, 'daily-local:20:00');
}

// 2. Legacy daily UTC rule
{
  const parsed = parseScheduleRule('daily:20:00');
  assert.equal(parsed.kind, 'daily');
  assert.equal(parsed.isLocal, false);
  assert.equal(parsed.dailyTime, '20:00');

  const desc = describeScheduleRule('daily:20:00');
  assert.match(desc.summary, /UTC/i);
  assert.equal(desc.isLocal, false);

  const formatted = formatScheduleRule({
    kind: 'daily',
    isLocal: false,
    dailyTime: '20:00',
  });
  assert.equal(formatted, 'daily:20:00');
}

// 3. Local cron rule
{
  const parsed = parseScheduleRule('local-cron:0 20 * * 1-5');
  assert.equal(parsed.kind, 'cron');
  assert.equal(parsed.isLocal, true);
  assert.equal(parsed.cronExpr, '0 20 * * 1-5');

  const desc = describeScheduleRule('local-cron:0 20 * * 1-5');
  assert.equal(desc.isLocal, true);
  assert.match(desc.detail, /local/i);

  const formatted = formatScheduleRule({
    kind: 'cron',
    isLocal: true,
    cronExpr: '0 20 * * 1-5',
  });
  assert.equal(formatted, 'local-cron:0 20 * * 1-5');
}

// 4. Legacy bare cron rule
{
  const parsed = parseScheduleRule('0 20 * * 1-5');
  assert.equal(parsed.kind, 'cron');
  assert.equal(parsed.isLocal, false);
  assert.equal(parsed.cronExpr, '0 20 * * 1-5');

  const desc = describeScheduleRule('0 20 * * 1-5');
  assert.equal(desc.isLocal, false);
  assert.match(desc.summary, /UTC/i);
}

// 5. Interval rule
{
  const parsed = parseScheduleRule('every:3600');
  assert.equal(parsed.kind, 'interval');
  assert.equal(parsed.intervalSecs, 3600);

  const desc = describeScheduleRule('every:3600');
  assert.equal(desc.summary, 'Every 1 hour');
}

// 6. Time formatting helper
{
  assert.equal(formatTime12h('20:00'), '8:00 PM');
  assert.equal(formatTime12h('08:05'), '8:05 AM');
  assert.equal(formatTime12h('00:00'), '12:00 AM');
  assert.equal(formatTime12h('12:00'), '12:00 PM');
}

console.log('All scheduleUtils tests passed successfully!');
