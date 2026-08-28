import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  expectedPersonalTokenStatus,
  requirePersonalToken,
  validTokenPage,
} from '../personal-token-admission.js';

const scenario = readFileSync(new URL('../k6/personal-token-admission.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../personal-token-admission.mjs', import.meta.url), 'utf8');
const token = `rsctf_pat_v1_${'a'.repeat(43)}`;

test('managed-token load contract preserves grammar and typed admission results', () => {
  assert.equal(requirePersonalToken(token, 'token'), token);
  assert.throws(() => requirePersonalToken('rsctf_pat_v1_short', 'token'));
  assert.equal(expectedPersonalTokenStatus('valid', 200), true);
  assert.equal(expectedPersonalTokenStatus('revoked', 401), true);
  assert.equal(expectedPersonalTokenStatus('random', 429, '1'), true);
  assert.equal(expectedPersonalTokenStatus('random', 429, ''), false);
  assert.equal(validTokenPage({ data: [], total: 4, length: 0 }), true);
  assert.equal(validTokenPage({ data: [1, 2], total: 2, length: 2 }), false);
});

test('managed-token scenario is fixed-rate read-only and keeps health independent', () => {
  assert.match(scenario, /\['valid', 'revoked', 'random', 'nat', 'multisource'\]/);
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /\/api\/tokens\?count=1&skip=0/);
  assert.match(scenario, /\/healthz/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  assert.match(runner, /PERSONAL_TOKEN_STRESS_ACK/);
  assert.match(runner, /ALLOW_REMOTE_PERSONAL_TOKEN_STRESS/);
  assert.match(runner, /mode: 0o600/);
  assert.match(runner, /fingerprint\(\)/);
  assert.match(runner, /finally \{/);
});
