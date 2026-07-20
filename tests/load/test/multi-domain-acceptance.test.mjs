import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  multiDomainLabels,
  normalizeMultiDomainScope,
  ownsMultiDomainResource,
  validateAcceptedAttackAttribution,
  validateAdScoreboardReconciliation,
  validateAdServiceMatrix,
  validateCleanupPasses,
  validateCrossHillRejection,
  validateFaultIsolation,
  validateFlagMatrix,
  validateKothCapabilityMatrix,
  validateManagedRuntimeOwnership,
  validateRoundCardinality,
} from '../multi-domain-acceptance.js';

const adChallenges = [11, 12];
const kothChallenges = [21, 22];
const participations = [31, 32];

test('multi-domain ownership is exact to run, game, challenge, and role', () => {
  const scope = normalizeMultiDomainScope('md-123', 7, [...adChallenges, ...kothChallenges]);
  const labels = multiDomainLabels(scope, 11, 'ad-service');
  assert.equal(ownsMultiDomainResource(labels, scope, 11, 'ad-service'), true);
  assert.equal(ownsMultiDomainResource(labels, scope, 12, 'ad-service'), false);
  assert.equal(ownsMultiDomainResource(labels, scope, 11, 'koth-bootstrap'), false);
  assert.throws(() => normalizeMultiDomainScope('UPPER', 7, [11, 12, 21, 22]), /DNS-safe/);
  assert.throws(() => multiDomainLabels(scope, 99, 'ad-service'), /outside/);
});

test('A&D matrix requires one distinct endpoint per challenge and every team cell', () => {
  const rows = [
    { challengeId: 11, participationId: 31, host: 'ad-one', port: 8080 },
    { challengeId: 11, participationId: 32, host: 'ad-one', port: 8080 },
    { challengeId: 12, participationId: 31, host: 'ad-two', port: 8080 },
    { challengeId: 12, participationId: 32, host: 'ad-two', port: 8080 },
  ];
  rows[1].host = 'ad-one-team-two';
  rows[3].host = 'ad-two-team-two';
  assert.deepEqual(validateAdServiceMatrix(rows, adChallenges, participations), {
    cells: 4,
    uniqueEndpoints: 4,
  });
  assert.throws(
    () => validateAdServiceMatrix(rows.map((row) => ({ ...row, host: 'shared' })), adChallenges, participations),
    /share endpoint/,
  );
  assert.throws(() => validateAdServiceMatrix(rows.slice(1), adChallenges, participations), /3\/4/);
});

test('managed runtime proof binds identity, installation labels, liveness, and no host port', () => {
  const id = 'a'.repeat(64);
  const scope = 'b'.repeat(32);
  const runtime = {
    Id: id,
    State: { Running: true },
    Config: { Labels: {
      'rsctf.managed': scope,
      'rsctf.scope': scope,
      'rsctf.launch-spec': 'c'.repeat(64),
    } },
    HostConfig: { PortBindings: {} },
    NetworkSettings: { Ports: { '8080/tcp': null } },
  };
  assert.equal(validateManagedRuntimeOwnership(runtime, id, scope), true);
  assert.throws(
    () => validateManagedRuntimeOwnership({ ...runtime, State: { Running: false } }, id, scope),
    /stopped/,
  );
  assert.throws(
    () => validateManagedRuntimeOwnership({ ...runtime, HostConfig: { PortBindings: { '8080/tcp': [{}] } } }, id, scope),
    /host port/,
  );
});

test('flag matrix rejects challenge leakage and attack attribution is exact', () => {
  const flags = [
    { challengeId: 11, participationId: 31, flag: 'flag{11_31}' },
    { challengeId: 11, participationId: 32, flag: 'flag{11_32}' },
    { challengeId: 12, participationId: 31, flag: 'flag{12_31}' },
    { challengeId: 12, participationId: 32, flag: 'flag{12_32}' },
  ];
  assert.deepEqual(validateFlagMatrix(flags, adChallenges, participations), { cells: 4, uniqueFlags: 4 });
  assert.throws(
    () => validateFlagMatrix(flags.map((row, index) => ({ ...row, flag: index ? row.flag : flags[1].flag })), adChallenges, participations),
    /reused/,
  );
  const attacks = [
    { serviceChallengeId: 11, flagChallengeId: 11, victimParticipationId: 32 },
    { serviceChallengeId: 12, flagChallengeId: 12, victimParticipationId: 32 },
  ];
  assert.deepEqual(validateAcceptedAttackAttribution(attacks, adChallenges, 32), {
    attacks: 2,
    challengeIds: [11, 12],
  });
  assert.throws(
    () => validateAcceptedAttackAttribution([{ ...attacks[0], flagChallengeId: 12 }, attacks[1]], adChallenges, 32),
    /another challenge/,
  );
});

test('round evidence is exactly four services and two hills without duplicate keys', () => {
  const snapshot = {
    flagCount: 4,
    flagServiceCount: 4,
    checkCount: 4,
    checkServiceCount: 4,
    deliveryCount: 4,
    deliveryServiceCount: 4,
    kothResultCount: 2,
    kothResultHillCount: 2,
    cycleCount: 2,
    activeCycleCount: 2,
    duplicateFlags: 0,
    duplicateChecks: 0,
    duplicateDeliveries: 0,
    duplicateKothResults: 0,
    duplicateCycles: 0,
    duplicateActiveCycles: 0,
  };
  assert.deepEqual(validateRoundCardinality(snapshot), { services: 4, hills: 2, duplicateKeys: 0 });
  assert.throws(() => validateRoundCardinality({ ...snapshot, duplicateChecks: 1 }), /duplicateChecks=1/);
  assert.throws(() => validateRoundCardinality({ ...snapshot, kothResultCount: 1 }), /kothResultCount=1/);
});

test('A&D scoreboard contains both services and reconciles per-service totals', () => {
  const model = {
    started: true,
    challenges: adChallenges.map((challengeId) => ({ challengeId })),
    teams: participations.map((participationId, index) => ({
      participationId,
      settledTotal: 30 + index,
      projectedTotal: 50 + index,
      services: [
        { challengeId: 11, settledPoints: 10, projectedPoints: 20 },
        { challengeId: 12, settledPoints: 20 + index, projectedPoints: 30 + index },
      ],
    })),
  };
  assert.deepEqual(validateAdScoreboardReconciliation(model, adChallenges, participations), {
    teams: 2,
    servicesPerTeam: 2,
    totalsReconciled: true,
  });
  const broken = structuredClone(model);
  broken.teams[0].projectedTotal += 1;
  assert.throws(() => validateAdScoreboardReconciliation(broken, adChallenges, participations), /do not reconcile/);
});

test('KotH capability matrix is a unique challenge/team cartesian product', () => {
  const rows = kothChallenges.flatMap((challengeId) => participations.map((participationId) => ({
    challengeId,
    participationId,
    token: `koth_scope_${challengeId}_${participationId}`,
    tokenChallengeId: challengeId,
    targetChallengeId: challengeId,
    cycleChallengeId: challengeId,
  })));
  assert.deepEqual(validateKothCapabilityMatrix(rows, kothChallenges, participations), {
    cells: 4,
    uniqueTokens: 4,
  });
  assert.throws(
    () => validateKothCapabilityMatrix(rows.map((row, index) =>
      index === 0 ? { ...row, targetChallengeId: 22 } : row), kothChallenges, participations),
    /crossed target\/cycle ownership/,
  );
  assert.throws(
    () => validateKothCapabilityMatrix(rows.map((row) => ({ ...row, token: 'koth_reused_token' })), kothChallenges, participations),
    /reused across scopes/,
  );
});

test('cross-hill marker must be healthy evidence without a scoped token match', () => {
  const result = {
    id: 90,
    gameId: 7,
    challengeId: 22,
    cycleId: 81,
    containerId: 'hill-destination',
    tokenWindowAttempt: 2,
    adRoundId: 44,
    roundGameId: 7,
    roundNumber: 12,
    plannedStartRound: 10,
    plannedEndRound: 21,
    markerObserved: true,
    status: 0,
    isScorable: true,
    tokenId: null,
    controller: null,
    responsible: null,
    sourceTokenMatches: 71,
    destinationMatches: 0,
  };
  const scope = {
    sourceTokenId: 71,
    sourceChallengeId: 21,
    destinationChallengeId: 22,
    gameId: 7,
    cycleId: 81,
    containerId: 'hill-destination',
    resetAttempt: 2,
  };
  assert.deepEqual(validateCrossHillRejection(result, scope), { rejected: true, resultId: 90 });
  assert.throws(
    () => validateCrossHillRejection({
      ...result,
      tokenId: 71,
      controller: 31,
      responsible: 31,
    }, scope),
    /accepted/,
  );
  for (const changed of [
    { cycleId: 82 },
    { containerId: 'another-hill' },
    { tokenWindowAttempt: 3 },
    { roundGameId: 8 },
  ]) {
    assert.throws(
      () => validateCrossHillRejection({ ...result, ...changed }, scope),
      /exact destination cycle scope/,
    );
  }
  assert.throws(
    () => validateCrossHillRejection({ ...result, roundNumber: 22 }, scope),
    /cycle round window/,
  );
  assert.throws(
    () => validateCrossHillRejection(result, { ...scope, sourceChallengeId: 22 }),
    /different challenge identities/,
  );
});

test('fault isolation requires the later hill to advance before lower recovery', () => {
  const result = validateFaultIsolation({
    lowerBefore: { phase: 'Active', readinessFailures: 0, updatedAtMs: 100 },
    higherBefore: { phase: 'Active', readinessFailures: 0, updatedAtMs: 100 },
    failedResponseStatus: 409,
    lowerAfter: { phase: 'ReadinessPending', readinessFailures: 1, updatedAtMs: 110 },
    higherAfter: { phase: 'Active', readinessFailures: 0, updatedAtMs: 120 },
    lowerRecovered: { phase: 'Active', readinessFailures: 1, updatedAtMs: 130 },
  });
  assert.deepEqual(result, { failIsolated: true, lowerRecovered: true });
  assert.throws(() => validateFaultIsolation({
    lowerBefore: { phase: 'Active', readinessFailures: 0 },
    higherBefore: { phase: 'Active', updatedAtMs: 100 },
    failedResponseStatus: 409,
    lowerAfter: { phase: 'ReadinessPending', readinessFailures: 1 },
    higherAfter: { phase: 'ReadinessPending', updatedAtMs: 100 },
    lowerRecovered: { phase: 'Active', readinessFailures: 1 },
  }), /starved/);
  assert.throws(() => validateFaultIsolation({
    lowerBefore: { phase: 'Active', readinessFailures: 0, updatedAtMs: 100 },
    higherBefore: { phase: 'Active', readinessFailures: 0, updatedAtMs: 100 },
    failedResponseStatus: 409,
    lowerAfter: { phase: 'ReadinessPending', readinessFailures: 1, updatedAtMs: 110 },
    higherAfter: { phase: 'Active', readinessFailures: 1, updatedAtMs: 120 },
    lowerRecovered: { phase: 'Active', readinessFailures: 1, updatedAtMs: 130 },
  }), /starved/);
});

test('cleanup needs two identical all-zero snapshots', () => {
  const clean = {
    games: 0,
    exactDatabaseRows: 0,
    runtimeContainers: 0,
    fixtureImages: 0,
    checkerDirectories: 0,
  };
  assert.equal(validateCleanupPasses([
    clean,
    { ...clean },
  ]), true);
  assert.throws(() => validateCleanupPasses([{}, {}]), /must contain exactly/);
  assert.throws(
    () => validateCleanupPasses([
      clean,
      { ...clean, exactDatabaseRows: 1 },
    ]),
    /retained exactDatabaseRows=1/,
  );
  const { fixtureImages: _omitted, ...missing } = clean;
  assert.throws(() => validateCleanupPasses([clean, missing]), /must contain exactly/);
  assert.throws(
    () => validateCleanupPasses([clean, { ...clean, unexpected: 0 }]),
    /must contain exactly/,
  );
});

test('started multi-domain fixtures use the exact guarded history-retention fallback', () => {
  const orchestrator = readFileSync(
    new URL('../multi-domain-acceptance.mjs', import.meta.url),
    'utf8',
  );
  const fixtures = readFileSync(new URL('../admin-fixtures.mjs', import.meta.url), 'utf8');
  assert.match(
    orchestrator,
    /removeOwnedAdFixtureContainers\(\);\s*deleteDisposableLoadGame\(state\.gameId, title, \{ runtimeIds: state\.runtimeIds \}\)/,
  );
  assert.match(
    orchestrator,
    /runtime\.Image === state\.fixtureImage\.imageId[\s\S]*rsctf\.load\.fixture[\s\S]*validateManagedRuntimeOwnership\(runtime, runtimeId, appDockerScope\)/,
  );
  assert.match(orchestrator, /state\.adRuntimeIds = managedAdRuntimeIds/);
  assert.match(
    orchestrator,
    /new Set\(\[\.\.\.state\.adRuntimeIds, \.\.\.durableRuntimeIds\]\)/,
  );
  assert.match(fixtures, /MULTI-DOMAIN-\[a-z0-9\]/);
  assert.match(
    orchestrator,
    /`\/api\/edit\/games\/\$\{state\.gameId\}\/ad\/koth\/\$\{lowerId\}\/recover`/,
  );
  assert.match(
    orchestrator,
    /`\/api\/stateful\/edit\/games\/\$\{state\.gameId\}\/ad\/koth\/\$\{lowerId\}\/recover`/,
  );
  assert.match(orchestrator, /checkerOwnershipViolationCount\(containerId\)/);
  assert.match(orchestrator, /Object\.values\(checkerOwnership\)\.every\(\(\{ violations \}\) => violations === 0\)/);
  assert.equal(
    (orchestrator.match(/state\.failure = String\(failure\?\.stack \|\| failure\?\.message \|\| failure\)/g) || []).length,
    2,
  );
});
