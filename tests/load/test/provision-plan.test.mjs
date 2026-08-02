import assert from 'node:assert/strict';
import test from 'node:test';

import { stagedEventSchedule } from '../provision-plan.js';

test('lifecycle configures future events before arming their live scoring window', () => {
  const now = 1_800_000_000_000;
  const duration = 30 * 60 * 1_000;
  const schedule = stagedEventSchedule(now, duration);

  assert.ok(schedule.stagingStart > now);
  assert.equal(schedule.stagingEnd - schedule.stagingStart, duration);
  assert.ok(schedule.liveStart < now);
  assert.ok(schedule.liveEnd > now);
  assert.equal(schedule.liveEnd - now, duration);
});

test('lifecycle schedule rejects invalid and overflowing timestamps', () => {
  assert.throws(() => stagedEventSchedule(Date.now(), 0), /safe positive integer/);
  assert.throws(
    () => stagedEventSchedule(Number.MAX_SAFE_INTEGER - 1_000, 2_000),
    /safe timestamp range/,
  );
});
