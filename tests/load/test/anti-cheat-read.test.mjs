import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ledgerFingerprint, validConditionalReport, validIncidentPage } from '../anti-cheat-read.js';

const scenario = readFileSync(new URL('../k6/anti-cheat-read.js', import.meta.url), 'utf8');

test('incident pages require ascending unique cursors and bounded rows', () => {
  const incident = (cursor) => ({ cursor, ownedTeam: {}, submitTeam: {}, submission: {} });
  assert.equal(validIncidentPage({ incidents: [incident(2), incident(3)], nextCursor: 3, hasMore: true }, 1, 10), true);
  assert.equal(validIncidentPage({ incidents: [incident(3), incident(2)], nextCursor: 3, hasMore: true }, 0, 10, false), true);
  assert.equal(validIncidentPage({ incidents: [incident(2), incident(2)], nextCursor: 2, hasMore: false }, 1, 10), false);
  assert.equal(validIncidentPage({ incidents: Array.from({ length: 101 }, (_, i) => incident(i + 1)), nextCursor: 101, hasMore: false }, 0, 10), false);
});

test('conditional reports accept only bounded 200, empty 304, or retryable 503', () => {
  const etag = `W/"rsctf-cheat-report-${'a'.repeat(64)}"`;
  assert.equal(validConditionalReport(304, 0, etag, ''), true);
  assert.equal(validConditionalReport(304, 1, etag, ''), false);
  assert.equal(validConditionalReport(503, 10, '', '2'), true);
  assert.equal(validConditionalReport(503, 10, '', '0'), false);
});

test('large-ledger scenario is fixed-rate, read-only, conditional, and health-gated', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  assert.match(scenario, /If-None-Match/);
  assert.match(scenario, /\/cheatinfo\/page\?after=/);
  assert.match(scenario, /\/healthz/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.equal(ledgerFingerprint('1|2|3|4|5|6'), '1|2|3|4|5|6');
  assert.throws(() => ledgerFingerprint('1|x'));
});
