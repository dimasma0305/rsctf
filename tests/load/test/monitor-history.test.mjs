import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/monitor-history.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../monitor-history.mjs', import.meta.url), 'utf8');
const routes = readFileSync(new URL('../../../src/controllers/game/routes.rs', import.meta.url), 'utf8');

test('monitor history and durable backfill use one fixed-rate bounded read per iteration', () => {
  assert.match(scenario, /executor:\s*'constant-arrival-rate'/);
  assert.match(scenario, /exec\.scenario\.iterationInTest/);
  assert.equal((scenario.match(/http\.get\(/g) || []).length, 1);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  for (const range of [
    'count=0',
    'count=1',
    'count=100',
    'count=10000',
    "encodeURIComponent('%_')",
    'longSearch',
    'events/backfill`',
    'events/backfill?after=0&limit=1',
    'events/backfill?after=0&limit=100',
    'events/backfill?after=0&limit=10000',
  ]) {
    assert.ok(scenario.includes(range), `missing monitor-history range ${range}`);
  }
  assert.match(scenario, /rows\.length > endpoint\.maxRows/);
  assert.match(scenario, /event\.cursor > previousCursor/);
  assert.match(scenario, /!ids\.has\(event\.id\)/);
  assert.match(scenario, /body\.nextCursor === rows\[rows\.length - 1\]\.cursor/);
  assert.doesNotMatch(scenario, /body\.data/);
  assert.match(scenario, /String\(response\.body \|\| ''\)\.length > 262144/);
  assert.match(scenario, /dropped_iterations:\s*\['count==0'\]/);
  assert.match(scenario, /server_5xx:\s*\['rate==0'\]/);
  assert.match(scenario, /rate_limited:\s*\['rate==0'\]/);
});

test('runner requires a large history and protects minted credentials', () => {
  assert.match(runner, /Number\.isSafeInteger\(eventCount\)/);
  assert.match(runner, /Number\.isSafeInteger\(durableEventCount\)/);
  assert.match(runner, /eventCount < 10000/);
  assert.match(runner, /durableEventCount !== eventCount/);
  assert.match(runner, /feed_cursor IS NOT NULL/);
  assert.match(runner, /submissionCount < 10000/);
  assert.match(runner, /WHERE role IN \(2,3\)/);
  assert.match(runner, /writeFileSync\(tokenFile, JSON\.stringify\(tokens\), \{ mode: 0o600 \}\)/);
  assert.match(runner, /rmSync\(tokenDirectory, \{ recursive: true, force: true \}\)/);
  assert.doesNotMatch(runner, /\b(?:INSERT|UPDATE|DELETE)\b/);
});

test('history and reconnect reads use named heavy-query admission', () => {
  for (const path of [
    '/api/game/{id}/events',
    '/api/game/{id}/events/backfill',
    '/api/game/{id}/submissions',
  ]) {
    const escaped = path.replace(/[{}]/g, '\\$&').replaceAll('/', '\\/');
    assert.match(routes, new RegExp(`"${escaped}"\\s*,\\s*limited\\(Policy::Query`));
  }
});
