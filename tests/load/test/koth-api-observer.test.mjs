import assert from 'node:assert/strict';
import { createHash, createHmac } from 'node:crypto';
import test from 'node:test';

import { kothApiEvidence } from '../applib.mjs';
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
