import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  maximumAdmittedObservations,
  validDecoyResponse,
} from '../honeypot-admission.js';

const scenario = readFileSync(new URL('../k6/honeypot-admission.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../honeypot-admission.mjs', import.meta.url), 'utf8');

test('honeypot responses remain indistinguishable while aggregate work is bounded', () => {
  assert.equal(validDecoyResponse(404, 'Not Found'), true);
  assert.equal(validDecoyResponse(429, 'Not Found'), false);
  assert.equal(maximumAdmittedObservations(10), 296);
});

test('honeypot fixed-rate gate covers source spray, health, SQL aggregation, and remote safety', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /'X-Real-IP': source/);
  assert.match(scenario, /response\.status >= 500/);
  assert.match(scenario, /\/healthz/);
  assert.match(runner, /HONEYPOT_STRESS_ACK/);
  assert.match(runner, /ALLOW_REMOTE_HONEYPOT_STRESS/);
  assert.match(runner, /"HoneypotHitBuckets"/);
  assert.match(runner, /"HoneypotBucketBudget"/);
  assert.match(runner, /next\.hits > 0/);
  assert.match(runner, /public decoys regressed to one-row-per-hit storage/);
});
