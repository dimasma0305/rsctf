import assert from 'node:assert/strict';
import { createHash, createHmac } from 'node:crypto';
import test from 'node:test';

import {
  assignUniqueKothApiCrown,
  isRetriableKothApiContextFailure,
  kothApiEvidence,
  validateKothApiContext,
} from '../applib.mjs';
import {
  kothObservationHeaders,
  kothObservationMessage,
  signKothObservation,
} from '../koth-api-observer.js';

const secret = `koth_api_${'a'.repeat(43)}`;
const timestamp = 1_785_130_000_123;
const body = '{"context":"abc","waves":[]}';

test('KotH API signatures bind the timestamp, game, challenge, and exact raw body', () => {
  const message = `${timestamp}.7.9.${body}`;
  assert.equal(kothObservationMessage(timestamp, 7, 9, body), message);
  assert.equal(
    signKothObservation(secret, timestamp, 7, 9, body),
    createHmac('sha256', secret).update(message).digest('hex'),
  );
  assert.notEqual(
    signKothObservation(secret, timestamp, 7, 9, body),
    signKothObservation(secret, timestamp, 7, 10, body),
  );
  assert.notEqual(
    signKothObservation(secret, timestamp, 7, 9, body),
    signKothObservation(secret, timestamp, 7, 9, `${body}\n`),
  );
});

test('KotH API headers use the documented wire names and sha256 prefix', () => {
  const headers = kothObservationHeaders(secret, timestamp, 7, 9, body);
  assert.equal(headers['x-rsctf-timestamp'], String(timestamp));
  assert.match(headers['x-rsctf-signature'], /^sha256=[0-9a-f]{64}$/);
});

test('KotH API success writes retry only transient context fences', () => {
  assert.equal(
    isRetriableKothApiContextFailure(
      new Error('fetch KotH API context → 409 {"title":"Leaderboard KotH context is not active"}'),
    ),
    true,
  );
  assert.equal(
    isRetriableKothApiContextFailure({
      status: 409,
      text: '{"title":"Leaderboard KotH context changed; fetch context and retry"}',
    }),
    true,
  );
  assert.equal(
    isRetriableKothApiContextFailure({
      status: 409,
      text: '{"title":"Leaderboard objective IDs and order are frozen for this challenge"}',
    }),
    false,
  );
  assert.equal(
    isRetriableKothApiContextFailure({ status: 401, text: 'Unauthorized' }),
    false,
  );
});

test('KotH API signing rejects ambiguous identities and oversized payloads', () => {
  assert.throws(() => kothObservationMessage(timestamp, 0, 9, body), /gameId/);
  assert.throws(() => kothObservationMessage('not-a-time', 7, 9, body), /timestamp/);
  assert.throws(
    () => kothObservationMessage(timestamp, 7, 9, 'x'.repeat(512 * 1024 + 1)),
    /512 KiB/,
  );
});

test('Leaderboard load evidence uses hashes and equivalent native score scales', () => {
  const small = kothApiEvidence('koth_team_small', 0);
  const large = kothApiEvidence('koth_team_large', 6);

  assert.equal(
    small.tokenHash,
    createHash('sha256').update('koth_team_small').digest('hex'),
  );
  assert.equal(Object.hasOwn(small, 'token'), false);
  assert.deepEqual(small.activity, large.activity);
  assert.equal(Object.hasOwn(small, 'integrity'), false);
  assert.equal(Object.hasOwn(large, 'integrity'), false);
  assert.deepEqual(small.activity, { earned: 1, possible: 1 });
  assert.equal(small.isCrown, false);
  assert.equal(
    small.objectives[0].earned / small.objectives[0].possible,
    large.objectives[0].earned / large.objectives[0].possible,
  );
  assert.deepEqual(small.objectives[0], { earned: 5, possible: 10 });
  assert.deepEqual(large.objectives[0], {
    earned: 5_000,
    possible: 10_000,
  });
});

test('Leaderboard load fixture crowns only one unique normalized leader', () => {
  const teams = [
    kothApiEvidence('koth_team_lower', 0),
    kothApiEvidence('koth_team_leader', 5),
  ];
  assignUniqueKothApiCrown(teams);
  assert.deepEqual(teams.map((team) => team.isCrown), [false, true]);
});

test('Leaderboard load fixture emits no Crown for equal native ratios on different scales', () => {
  const teams = [
    kothApiEvidence('koth_team_small_scale', 0),
    kothApiEvidence('koth_team_large_scale', 6),
  ];
  teams[0].isCrown = true;
  assignUniqueKothApiCrown(teams);
  assert.deepEqual(teams.map((team) => team.isCrown), [false, false]);
});

test('Leaderboard load fixture resolves ties after platform-equivalent integer normalization', () => {
  const row = (earned, possible, isCrown) => ({
    activity: { earned: 1, possible: 1 },
    objectives: [{ earned, possible }],
    isCrown,
  });
  const teams = [row(1, 3, true), row(333_333, 1_000_000, false)];
  assignUniqueKothApiCrown(teams);
  assert.deepEqual(teams.map((team) => team.isCrown), [false, false]);
});

test('Leaderboard load fixture never crowns zero or incomplete evidence', () => {
  const teams = [
    {
      activity: { earned: 0, possible: 1 },
      objectives: [{ earned: 1, possible: 1 }],
      isCrown: true,
    },
    {
      activity: { earned: 1, possible: 1 },
      objectives: [{ earned: 0, possible: 1 }],
      isCrown: true,
    },
  ];
  assignUniqueKothApiCrown(teams);
  assert.deepEqual(teams.map((team) => team.isCrown), [false, false]);
});

test('Leaderboard load fixture requires a complete fenced context window', () => {
  const context = {
    apiVersion: 'v1',
    context: 'a'.repeat(64),
    cycleNumber: 3,
    resetAttempt: 1,
    roundNumber: 7,
    cycleEndsAt: 240_000,
    waveWindowStartsAt: 120_000,
    waveWindowEndsAt: 180_000,
    generatedAt: 125_000,
    eligibleTokenHashes: ['b'.repeat(64), 'c'.repeat(64)],
  };
  assert.equal(validateKothApiContext(context), context);
  for (const malformed of [
    { ...context, apiVersion: 'v2' },
    { ...context, cycleEndsAt: undefined },
    { ...context, cycleEndsAt: context.waveWindowEndsAt - 1 },
    { ...context, waveWindowEndsAt: context.waveWindowStartsAt },
    { ...context, eligibleTokenHashes: ['b'.repeat(64), 'b'.repeat(64)] },
  ]) {
    assert.throws(() => validateKothApiContext(malformed), /context response is malformed/);
  }
});
