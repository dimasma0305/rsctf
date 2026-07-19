import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

import {
  distinctPlausibleFlags,
  inspectDistinctFlagBatch,
  inspectDistinctKnownFlagBatch,
  inspectKnownFlagBatch,
  MAX_AD_BATCH,
  parseAdSubmitBatchConfig,
} from '../ad-submit-batch.js';

const FLAG = 'flag{known-batch-test}';

function environment(overrides = {}) {
  return {
    CONFIRM_MUTATING_LOAD: '1',
    TARGET: 'http://127.0.0.1:8080',
    GAME: '42',
    TOKEN: 'ad_test_token',
    FLAG,
    ...overrides,
  };
}

function result(status, plantedRound = 19) {
  return {
    flag: FLAG,
    status,
    flagPlantedAtRound: plantedRound,
    message: null,
  };
}

test('known-flag batch defaults are deliberately low and fixed-rate', () => {
  assert.deepEqual(parseAdSubmitBatchConfig(environment()), {
    target: 'http://127.0.0.1:8080',
    gameId: 42,
    token: 'ad_test_token',
    flag: FLAG,
    batchShape: 'repeated',
    explicitFlags: null,
    rate: 1,
    vus: 4,
    maxVus: 4,
    duration: '30s',
    requestTimeout: '10s',
  });
  assert.equal(MAX_AD_BATCH, 100);
});

test('known-flag batch requires explicit mutation acknowledgement and secrets', () => {
  assert.throws(
    () => parseAdSubmitBatchConfig(environment({ CONFIRM_MUTATING_LOAD: '0' })),
    /CONFIRM_MUTATING_LOAD=1/,
  );
  assert.throws(() => parseAdSubmitBatchConfig(environment({ GAME: '' })), /GAME is required/);
  assert.throws(() => parseAdSubmitBatchConfig(environment({ TOKEN: '' })), /TOKEN is required/);
  assert.throws(() => parseAdSubmitBatchConfig(environment({ FLAG: '' })), /FLAG is required/);
});

test('known-flag batch bounds mutating load and rejects unsafe header or target input', () => {
  assert.throws(() => parseAdSubmitBatchConfig(environment({ RATE: '21' })), /RATE/);
  assert.throws(() => parseAdSubmitBatchConfig(environment({ DURATION: '11m' })), /DURATION/);
  assert.throws(() => parseAdSubmitBatchConfig(environment({ TOKEN: 'bad token' })), /TOKEN/);
  assert.throws(
    () => parseAdSubmitBatchConfig(environment({ BATCH_SHAPE: 'mixed' })),
    /BATCH_SHAPE/,
  );
  assert.throws(
    () => parseAdSubmitBatchConfig(environment({ TARGET: 'https://user:pass@example.test' })),
    /TARGET/,
  );
});

test('distinct batch uses 100 unique engine-shaped unknown flags', () => {
  const flags = distinctPlausibleFlags();
  assert.equal(flags.length, MAX_AD_BATCH);
  assert.equal(new Set(flags).size, MAX_AD_BATCH);
  for (const flag of flags) {
    assert.match(flag, /^flag\{[A-Za-z0-9_-]{32}\}$/);
  }
  const inspected = inspectDistinctFlagBatch(
    {
      acceptedCount: 0,
      results: flags.map((flag) => ({
        flag,
        status: 'wrong',
        flagPlantedAtRound: null,
        message: null,
      })),
    },
    flags,
  );
  assert.equal(inspected.valid, true);
  assert.equal(inspected.counts.wrong, MAX_AD_BATCH);
});

test('distinct batch rejects reordered or non-wrong results', () => {
  const flags = distinctPlausibleFlags();
  const rows = flags.map((flag) => ({
    flag,
    status: 'wrong',
    flagPlantedAtRound: null,
    message: null,
  }));
  [rows[0], rows[1]] = [rows[1], rows[0]];
  assert.equal(inspectDistinctFlagBatch({ acceptedCount: 0, results: rows }, flags).valid, false);
});

test('distinct-known batch requires and validates an explicit 100-flag fixture', () => {
  const flags = distinctPlausibleFlags();
  const config = parseAdSubmitBatchConfig(
    environment({
      BATCH_SHAPE: 'distinct-known',
      FLAGS_JSON: JSON.stringify(flags),
    }),
  );
  assert.deepEqual(config.explicitFlags, flags);
  assert.throws(
    () =>
      parseAdSubmitBatchConfig(
        environment({ BATCH_SHAPE: 'distinct-known', FLAGS_JSON: '[]' }),
      ),
    /exactly 100 distinct/,
  );

  const inspected = inspectDistinctKnownFlagBatch(
    {
      acceptedCount: 0,
      results: flags.map((flag) => ({
        flag,
        status: 'duplicate',
        flagPlantedAtRound: 19,
        message: null,
      })),
    },
    flags,
  );
  assert.equal(inspected.valid, true);
  assert.equal(inspected.counts.duplicate, MAX_AD_BATCH);
});

test('known-flag batch accepts one capture followed by duplicates', () => {
  const inspected = inspectKnownFlagBatch(
    {
      acceptedCount: 1,
      results: [result('accepted'), ...Array.from({ length: 99 }, () => result('duplicate'))],
    },
    FLAG,
  );
  assert.equal(inspected.valid, true);
  assert.equal(inspected.counts.accepted, 1);
  assert.equal(inspected.counts.duplicate, 99);
});

test('known-flag batch accepts an already captured all-duplicate batch', () => {
  const inspected = inspectKnownFlagBatch(
    {
      acceptedCount: 0,
      results: Array.from({ length: MAX_AD_BATCH }, () => result('duplicate')),
    },
    FLAG,
  );
  assert.equal(inspected.valid, true);
  assert.equal(inspected.counts.accepted, 0);
  assert.equal(inspected.counts.duplicate, MAX_AD_BATCH);
});

test('known-flag batch rejects inactive, self, stale, or inconsistent semantics', () => {
  for (const status of ['wrong', 'expired', 'self_attack', 'not_started', 'ended', 'paused', 'rejected']) {
    const rows = Array.from({ length: MAX_AD_BATCH }, () => result('duplicate'));
    rows[0] = result(status);
    assert.equal(inspectKnownFlagBatch({ acceptedCount: 0, results: rows }, FLAG).valid, false);
  }
  assert.equal(
    inspectKnownFlagBatch(
      {
        acceptedCount: 1,
        results: Array.from({ length: MAX_AD_BATCH }, () => result('duplicate')),
      },
      FLAG,
    ).valid,
    false,
  );
});

test('runner never imports discovery helpers or prints supplied flag and token', () => {
  const runner = readFileSync(new URL('../ad-submit-batch.mjs', import.meta.url), 'utf8');
  assert.doesNotMatch(runner, /from ['"]\.\/lib\.mjs/);
  assert.doesNotMatch(runner, /\b(?:discover|mintJwt|sql|docker)\s*\(/);
  assert.doesNotMatch(runner, /console\.(?:log|error)\([^)]*config\.(?:flag|token)/s);
  assert.match(runner, /credentials redacted/);
});
