import assert from 'node:assert/strict';
import test from 'node:test';

import { retryTransientUntil } from '../lib.mjs';

test('bounded retry caps request timeouts and sleeps to one monotonic deadline', async () => {
  let currentMs = 0;
  const timeouts = [];
  const deadlines = [];
  const waits = [];
  const transient = { status: 409 };
  const result = await retryTransientUntil(
    async ({ deadlineMs, timeoutMs }) => {
      deadlines.push(deadlineMs);
      timeouts.push(timeoutMs);
      currentMs += Math.min(4_600, timeoutMs);
      return transient;
    },
    (value) => value === transient,
    {
      budgetMs: 10_000,
      delayMs: 500,
      now: () => currentMs,
      wait: async (delayMs) => {
        waits.push(delayMs);
        currentMs += delayMs;
      },
    },
  );

  assert.equal(result, transient);
  assert.deepEqual(timeouts, [10_000, 4_900]);
  assert.deepEqual(deadlines, [10_000, 10_000]);
  assert.deepEqual(waits, [500, 300]);
  assert.equal(currentMs, 10_000);
});

test('bounded retry rethrows the exact transient error at its deadline', async () => {
  let currentMs = 0;
  const expected = new Error('transient');
  await assert.rejects(
    retryTransientUntil(
      async ({ timeoutMs }) => {
        currentMs += timeoutMs;
        throw expected;
      },
      (value) => value === expected,
      { budgetMs: 10_000, now: () => currentMs, wait: async () => {} },
    ),
    (error) => error === expected,
  );
  assert.equal(currentMs, 10_000);
});

test('bounded retry returns a non-transient result without sleeping', async () => {
  let waited = false;
  const result = await retryTransientUntil(
    async ({ timeoutMs }) => ({ status: 200, timeoutMs }),
    (value) => value?.status === 409,
    { wait: async () => { waited = true; } },
  );
  assert.equal(result.status, 200);
  assert.ok(result.timeoutMs > 0 && result.timeoutMs <= 10_000);
  assert.equal(waited, false);
});
