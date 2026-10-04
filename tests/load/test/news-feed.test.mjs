import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/news-feed.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../news-feed.mjs', import.meta.url), 'utf8');

test('news-feed load is fixed-rate, read-only, bounded, and conditional', () => {
  assert.match(scenario, /executor:\s*'constant-arrival-rate'/);
  assert.equal((scenario.match(/http\.get\(/g) || []).length, 2);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  assert.match(scenario, /\/api\/posts\/latest/);
  assert.match(scenario, /posts\.length > 20/);
  assert.match(scenario, /'If-None-Match': etag/);
  assert.match(scenario, /response\.status !== 304/);
  assert.match(scenario, /String\(response\.body \|\| ''\)\.length !== 0/);
  assert.match(scenario, /dropped_iterations:\s*\['count==0'\]/);
  assert.match(scenario, /server_5xx:\s*\['rate==0'\]/);
});

test('news-feed runner requires acknowledgement and exact health before and after', () => {
  assert.match(runner, /NEWS_FEED_STRESS_ACK/);
  assert.match(runner, /RATE must be an integer between 1 and 500/);
  assert.equal((runner.match(/exactHealth\(\)/g) || []).length, 2);
  assert.doesNotMatch(runner, /\b(?:INSERT|UPDATE|DELETE)\b/);
});
