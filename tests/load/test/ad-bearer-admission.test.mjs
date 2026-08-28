import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { expectedBearerStatus, requireAdToken, validTargetModel } from '../ad-bearer-admission.js';
import { tokenHash } from '../ad-bearer-fixture.mjs';

const scenario = readFileSync(new URL('../k6/ad-bearer-admission.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../ad-bearer-admission.mjs', import.meta.url), 'utf8');
const token = `ad_${'a'.repeat(43)}`;

test('A&D bearer contracts enforce fixed shape, hashing, and response classes', () => {
  assert.equal(requireAdToken(token, 'token'), token);
  assert.match(tokenHash(token), /^[a-f0-9]{64}$/);
  assert.throws(() => requireAdToken('ad_short', 'token'));
  assert.equal(expectedBearerStatus('valid', 200), true);
  assert.equal(expectedBearerStatus('revoked', 401), true);
  assert.equal(expectedBearerStatus('slow', 503, '2'), true);
  assert.equal(expectedBearerStatus('slow', 503, ''), true);
  assert.equal(validTargetModel({ currentRound: 1, challenges: [{ challengeId: 2, title: 'svc', teams: [] }] }), true);
});

test('A&D scenario covers valid/revoked/random/NAT/many-source and bounded outage phases', () => {
  assert.match(scenario, /\['valid', 'revoked', 'random', 'nat', 'multisource'/);
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /MODE === 'loop'/);
  assert.match(scenario, /ad_bearer_slow_timeouts/);
  assert.match(scenario, /\['count>0'\]/);
  assert.match(scenario, /\/api\/Game\/\$\{GAME\}\/Ad\/Targets/);
  assert.match(scenario, /\/livez/);
  assert.match(scenario, /\/healthz/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  assert.match(runner, /CONFIRM_AD_REDIS_OUTAGE/);
  assert.match(runner, /AD_BEARER_STRESS_ACK/);
  assert.match(runner, /ALLOW_REMOTE_AD_BEARER_STRESS/);
  assert.match(runner, /finally \{/);
  assert.match(runner, /LOCK TABLE "AdTeamApiTokens" IN ACCESS EXCLUSIVE MODE/);
  assert.match(runner, /CONFIRM_AD_SLOW_POOL/);
  assert.match(runner, /run\('loop', 'loop'\)/);
  assert.match(runner, /fingerprint\(\)/);
});
