import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/public-security.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../public-security.mjs', import.meta.url), 'utf8');

test('public security load is fixed-rate, multi-source, health isolated, and read-only', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /\/api\/captcha\/powchallenge/);
  assert.match(scenario, /\/api\/team\/verify/);
  assert.match(scenario, /FIXTURE\.trusted/);
  assert.match(scenario, /FIXTURE\.attacker/);
  assert.match(scenario, /response\.status === 429/);
  assert.match(scenario, /Retry-After/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(scenario, /\/healthz/);
  assert.doesNotMatch(scenario, /http\.(?:put|patch|del|delete)\(/);
});

test('runner requires explicit remote acknowledgement and preserves credentials', () => {
  assert.match(runner, /PUBLIC_SECURITY_STRESS_ACK/);
  assert.match(runner, /ALLOW_REMOTE_PUBLIC_SECURITY_STRESS/);
  assert.match(runner, /participation\.status=1/);
  assert.match(runner, /generateKeyPairSync\('ed25519'\)/);
  assert.match(runner, /fingerprint\(\)/);
  assert.match(runner, /finally \{/);
});
