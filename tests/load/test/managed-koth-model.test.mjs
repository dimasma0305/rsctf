import assert from 'node:assert/strict';
import test from 'node:test';

import {
  managedKothAbusePlan,
  managedKothHarnessConfig,
  managedKothLoadPlan,
  managedKothOperationCycleId,
  managedKothSummaryMetric,
  validateManagedKothRecovery,
  validateManagedKothIntegrity,
  validateManagedReporterEnvironment,
  validateManagedReporterStatus,
} from '../managed-koth-model.js';

test('managed KotH retained metrics accept k6 1.x and 2.x summary shapes', () => {
  const k6v1 = {
    metrics: {
      server_5xx: { values: { rate: 0 } },
      valid_capabilities_exercised: { values: { count: 2_000 } },
    },
  };
  const k6v2 = {
    metrics: {
      server_5xx: { value: 0, passes: 0, fails: 2_000 },
      valid_capabilities_exercised: { count: 2_000 },
    },
  };
  assert.equal(managedKothSummaryMetric(k6v1, 'server_5xx', 'rate'), 0);
  assert.equal(managedKothSummaryMetric(k6v2, 'server_5xx', 'rate'), 0);
  assert.equal(managedKothSummaryMetric(k6v1, 'valid_capabilities_exercised', 'count'), 2_000);
  assert.equal(managedKothSummaryMetric(k6v2, 'valid_capabilities_exercised', 'count'), 2_000);
  assert.ok(Number.isNaN(managedKothSummaryMetric({}, 'server_5xx', 'rate')));
});

test('managed KotH load plan covers every 2k capability at a fixed rate with bounded concurrency', () => {
  const plan = managedKothLoadPlan();
  assert.equal(plan.rosterSize, 2_000);
  assert.equal(plan.activeFleet, 64);
  assert.equal(plan.scheduledIterations, 2_000);
  assert.equal(plan.defaultAdmissionPerMinute, 30_000);
  assert.equal(plan.defaultAdmissionRefillPerSecond, 500);
  assert.ok(plan.rate < plan.defaultAdmissionRefillPerSecond);
  assert.throws(() => managedKothLoadPlan({ rosterSize: 1_999 }), /exact 2000-team roster/);
  assert.throws(() => managedKothLoadPlan({ activeFleet: 129 }), /between 2 and 128/);
  assert.throws(
    () => managedKothLoadPlan({ rate: 10, durationSeconds: 20 }),
    /schedule exactly 2000 plays/,
  );
  assert.throws(
    () => managedKothLoadPlan({ rate: 1_000, durationSeconds: 2 }),
    /default capability-admission refill/,
  );
});

test('managed KotH abuse plan must exhaust even a full configured source bucket', () => {
  const plan = managedKothAbusePlan();
  assert.equal(plan.scheduledIterations, 6_000);
  assert.equal(plan.maximumAdmittedFromFullBucket, 4_500);
  assert.ok(plan.scheduledIterations > plan.maximumAdmittedFromFullBucket);
  assert.throws(
    () => managedKothAbusePlan({ rate: 100, durationSeconds: 30 }),
    /cannot exhaust a full 3000\/minute/,
  );
  assert.throws(
    () => managedKothAbusePlan({ admissionPerMinute: 2_999 }),
    /between 3000 and 1000000/,
  );
});

test('managed reporter operation discovery accepts credential-fenced identities only', () => {
  assert.equal(managedKothOperationCycleId('koth-cycle:41:attempt:3'), 41);
  assert.equal(
    managedKothOperationCycleId(
      `koth-cycle:41:attempt:3:managed-reporter-v2:${'a'.repeat(16)}:${'b'.repeat(32)}`,
    ),
    41,
  );
  assert.equal(managedKothOperationCycleId('koth-cycle:41:attempt:3:managed-reporter-v2:secret:secret'), null);
  assert.equal(managedKothOperationCycleId('other:41'), null);
});

test('destructive harness accepts only an acknowledged loopback target with retained artifacts', () => {
  const config = managedKothHarnessConfig({
    MANAGED_KOTH_STRESS_ACK: '1',
    MANAGED_KOTH_DISPOSABLE: '1',
    TARGET: 'http://127.0.0.1:8080',
    SUMMARY_JSON: '/tmp/managed-koth.json',
    RESOURCE_JSON: '/tmp/managed-koth-resources.json',
  });
  assert.equal(config.target, 'http://127.0.0.1:8080');
  assert.throws(
    () => managedKothHarnessConfig({
      MANAGED_KOTH_STRESS_ACK: '1',
      MANAGED_KOTH_DISPOSABLE: '1',
      TARGET: 'https://tcp.1pc.tf',
      SUMMARY_JSON: '/tmp/managed-koth.json',
      RESOURCE_JSON: '/tmp/managed-koth-resources.json',
    }),
    /loopback/,
  );
});

test('managed reporter injection is exact and recovery rotates only its lifecycle generation', () => {
  const base = 'http://rsctf-koth-reporter:8080';
  const injected = validateManagedReporterEnvironment([
    'RSCTF_KOTH_GAME_ID=7',
    'RSCTF_KOTH_CHALLENGE_ID=9',
    `RSCTF_KOTH_PLATFORM_URL=${base}`,
    `RSCTF_KOTH_CONTEXT_URL=${base}/api/v1/koth/games/7/challenges/9/context`,
    `RSCTF_KOTH_OBSERVATION_URL=${base}/api/v1/koth/games/7/challenges/9/observations`,
    `RSCTF_KOTH_REPORTER_SECRET=koth_target_${'x'.repeat(43)}`,
  ], { gameId: 7, challengeId: 9, platformUrl: base });
  assert.match(injected.RSCTF_KOTH_REPORTER_SECRET, /^koth_target_/);
  const after = {
    cycleId: 41,
    resetAttempt: 3,
    containerId: 'new',
    credentialRevision: 'b'.repeat(32),
    operation: `koth-cycle:41:attempt:3:managed-reporter-v2:${'c'.repeat(16)}:${'b'.repeat(32)}`,
  };
  assert.equal(validateManagedKothRecovery({
    cycleId: 41,
    resetAttempt: 2,
    containerId: 'old',
    credentialRevision: 'a'.repeat(32),
  }, after), after);
  assert.throws(
    () => validateManagedKothRecovery({ cycleId: 41, resetAttempt: 2, containerId: 'old', credentialRevision: 'a' }, { ...after, resetAttempt: 2 }),
    /recovery identity failed/,
  );
});

test('reporter status is bounded, secret-free, and proves all capability identities', () => {
  const status = {
    reporterConfigured: true,
    reporterHealthy: true,
    successfulReports: 2,
    submittedWaves: 4,
    contextRefreshes: 8,
    eligibleRoster: 2_000,
    uniqueAuthenticated: 2_000,
    uniqueActivePlayed: 64,
    invalidAuthentications: 3,
    lastRound: 9,
    lastContext: 'a'.repeat(64),
    lastError: null,
  };
  assert.equal(validateManagedReporterStatus(status, {}), status);
  assert.throws(
    () => validateManagedReporterStatus({ ...status, reporterSecret: 'never' }),
    /forbidden fields/,
  );
  assert.throws(
    () => validateManagedReporterStatus({ ...status, uniqueAuthenticated: 1_999 }, {}),
    /1999\/2000/,
  );
});

test('reporter status retains a bounded secret-free unhealthy reason', () => {
  const status = {
    reporterConfigured: true,
    reporterHealthy: false,
    successfulReports: 0,
    submittedWaves: 0,
    contextRefreshes: 1,
    eligibleRoster: 2_000,
    uniqueAuthenticated: 2_000,
    uniqueActivePlayed: 64,
    invalidAuthentications: 0,
    lastRound: 9,
    lastContext: 'a'.repeat(64),
    lastError: 'HTTP 409',
  };
  assert.equal(validateManagedReporterStatus(status), status);
  assert.throws(
    () => validateManagedReporterStatus(status, {}),
    /reporter is unhealthy: HTTP 409/,
  );
  assert.throws(
    () => validateManagedReporterStatus({ ...status, lastError: null }),
    /status is malformed/,
  );
  assert.throws(
    () => validateManagedReporterStatus({ ...status, lastError: 'x'.repeat(301) }),
    /status is malformed/,
  );
  assert.throws(
    () => validateManagedReporterStatus({ ...status, lastError: 'HTTP 409\nforged' }),
    /status is malformed/,
  );
});

test('integrity contract requires separately reported dense 2k waves and exact zeroes', () => {
  const evidence = {
    rosterCount: 2_000,
    capabilityCount: 2_000,
    pendingRevocations: 0,
    scorableRounds: 2,
    denseRows: 4_000,
    zeroRows: 3_872,
    positiveRows: 128,
    crownRows: 2,
    uniqueCrownRounds: 2,
    crownMismatches: 0,
    denseRounds: 2,
    fullRosterWaves: 2,
    invalidRows: 0,
    exclusiveRows: 0,
    duplicateRows: 0,
    snapshotRows: 2_000,
    snapshotWaves: 1,
    reporterResetAttempt: 1,
    reporterUsed: true,
  };
  assert.equal(
    validateManagedKothIntegrity(evidence, { minimumScorableRounds: 2, minimumResetAttempts: 1 }),
    evidence,
  );
  assert.throws(
    () => validateManagedKothIntegrity({ ...evidence, denseRows: 3_999 }),
    /integrity failed/,
  );
  assert.throws(
    () => validateManagedKothIntegrity({ ...evidence, zeroRows: 3_871 }),
    /integrity failed/,
  );
});
