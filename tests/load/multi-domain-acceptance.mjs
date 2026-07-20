// Focused destructive acceptance for the domain cardinality the full lifecycle
// does not cover: two A&D services and two independent KotH hills in one game.
// It is intentionally functional, not a throughput benchmark.
import { randomUUID } from 'node:crypto';

import * as A from './applib.mjs';
import {
  acquireAdminLifecycleDatabaseLock,
  deleteDisposableLoadGame,
  inspectUnchangedServerRuntimeIdentity,
  inspectUniformServerRuntimeIdentity,
  originalServerRuntimeLogTargets,
  persistRecovery,
  removeRecovery,
  sqlLiteral,
  unwrap,
} from './admin-fixtures.mjs';
import { assertSafeAdminTarget } from './admin-lifecycle.js';
import { dockerScopeFromContainerEnv } from './docker-scope.js';
import {
  assertDisposableEditStack,
  assertRuntimeRoles,
  discoverManagedKothHill,
  requireCondition,
} from './edit-lifecycle-fixtures.mjs';
import { kothContainerOverride } from './fixture-image-config.js';
import { materializeFixtures } from './fixtures.mjs';
import { docker, mintJwt, PG, RSCTF, sleep, sql, TARGET } from './lib.mjs';
import { countContainerFatalLogs } from './log-audit.mjs';
import {
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
} from './multi-domain-acceptance.js';
import {
  acquireExclusiveProcessLock,
  loadOrchestrationLockPath,
} from './process-control.mjs';

const TEAM_COUNT = 2;
const AD_CHALLENGE_COUNT = 2;
const KOTH_CHALLENGE_COUNT = 2;
const AD_NET = process.env.AD_NET || 'rsctf-ad';
const runId = String(
  process.env.MULTI_DOMAIN_RUN_ID ||
    `md-${Date.now().toString(36)}-${process.pid.toString(36)}`,
).trim();
if (!/^[a-z0-9][a-z0-9-]{0,31}$/.test(runId)) {
  throw new Error('MULTI_DOMAIN_RUN_ID must contain 1-32 lowercase letters, digits, or hyphens');
}
const title = `MULTI-DOMAIN-${runId}`;
const recoveryPath = `/tmp/rsctf-multi-domain-${runId}.json`;
const keepManifest = process.env.KEEP_MULTI_DOMAIN_MANIFEST === '1';
const markerTimeoutSeconds = boundedIntegerEnv('MULTI_DOMAIN_OBSERVATION_TIMEOUT_SECONDS', 90, 30, 240);
const cleanupStabilityMs = boundedIntegerEnv('MULTI_DOMAIN_CLEANUP_STABILITY_MS', 2_000, 1_000, 10_000);
const kothOverride = kothContainerOverride(process.env);
const rawWebTargets = String(process.env.WEB_TARGETS || '').trim();
const webTargets = (rawWebTargets.startsWith('[') ? JSON.parse(rawWebTargets) : rawWebTargets.split(','))
  .map((target) => String(target).trim().replace(/\/$/, ''))
  .filter(Boolean);
const controlTarget = String(process.env.CONTROL_TARGET || '').trim().replace(/\/$/, '');
const serverContainers = [...new Set([
  RSCTF,
  ...String(process.env.ADMIN_RSCTF_CONTAINERS || '')
    .split(',')
    .map((name) => name.trim())
    .filter(Boolean),
])];

const state = {
  schemaVersion: 1,
  runId,
  title,
  target: TARGET,
  startedAt: Date.now(),
  completed: false,
  gameId: null,
  adChallengeIds: [],
  kothChallengeIds: [],
  challengeIds: [],
  userIds: [],
  teamIds: [],
  participationIds: [],
  serviceIds: [],
  targetIds: [],
  cycleIds: [],
  fixtureImage: null,
  adRuntimeIds: [],
  runtimeIds: [],
  evidence: {},
  cleanup: null,
  failure: null,
};

let processLock;
let databaseLock;
let appDockerScope;
let originalLowerChecker = null;

function boundedIntegerEnv(name, fallback, minimum, maximum) {
  const value = Number(process.env[name] ?? fallback);
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new Error(`${name} must be an integer from ${minimum} through ${maximum}`);
  }
  return value;
}

function saveRecovery() {
  persistRecovery(recoveryPath, state);
}

function parseJson(value, label) {
  try {
    return JSON.parse(String(value || ''));
  } catch (error) {
    throw new Error(`${label} returned malformed JSON: ${error.message}`);
  }
}

function jsonRows(query, label) {
  return parseJson(sql(`SELECT COALESCE(json_agg(row_to_json(result)),'[]'::json)::text FROM (${query}) result`), label);
}

function mustDocker(result, label) {
  if (result.status !== 0) {
    throw new Error(`${label}: ${String(result.stderr || result.error?.message || 'Docker command failed').trim()}`);
  }
  return result;
}

function inspectContainer(reference) {
  const inspected = docker(['inspect', String(reference)]);
  if (inspected.status !== 0) {
    if (/no such (?:container|object)/i.test(String(inspected.stderr || ''))) return null;
    throw new Error(`inspect ${reference}: ${String(inspected.stderr || '').trim()}`);
  }
  const records = parseJson(inspected.stdout, `Docker inspection for ${reference}`);
  requireCondition(Array.isArray(records) && records.length === 1, `Docker inspection for ${reference} is ambiguous`);
  return records[0];
}

function configuredRuntimeRole(reference) {
  const inspected = inspectContainer(reference);
  requireCondition(inspected, `server ${reference} disappeared before its role audit`);
  const roles = (inspected.Config?.Env || [])
    .filter((entry) => String(entry).startsWith('RSCTF_ROLE='))
    .map((entry) => String(entry).slice('RSCTF_ROLE='.length));
  requireCondition(
    roles.length === 1 && ['all', 'web', 'control', 'engine', 'network'].includes(roles[0]),
    `server ${reference} has an ambiguous runtime role`,
  );
  return roles[0];
}

function checkerOwnershipViolationCount(reference) {
  const logs = mustDocker(
    docker(
      ['logs', '--since', new Date(state.startedAt).toISOString(), reference],
      { maxBuffer: 64 * 1024 * 1024 },
    ),
    `audit checker ownership logs for ${reference}`,
  );
  return `${logs.stdout || ''}\n${logs.stderr || ''}`
    .split(/\r?\n/)
    .filter((line) =>
      /iptables: firewall rule check|checker egress isolation failed|checker process confinement failed/i.test(line))
    .length;
}

function discoverAppDockerScope() {
  const inspected = mustDocker(
    docker(['inspect', RSCTF, '--format', '{{json .Config.Env}}']),
    'discover RSCTF Docker scope',
  );
  return dockerScopeFromContainerEnv(parseJson(inspected.stdout.trim(), 'RSCTF environment'));
}

function buildManagedAdImage() {
  const tag = 'rsctf-load-ad:multi-domain-v1';
  const existing = docker(['image', 'inspect', tag]);
  const preexisting = existing.status === 0;
  const baseImage = mustDocker(
    docker(['inspect', RSCTF, '--format', '{{.Config.Image}}']),
    'discover base image for managed A&D fixture',
  ).stdout.trim();
  const fixtures = materializeFixtures();
  mustDocker(docker([
    'build', '--pull=false', '--tag', tag,
    '--file', fixtures.adDockerfile,
    '--build-arg', `BASE_IMAGE=${baseImage}`,
    fixtures.root,
  ]), 'build managed A&D fixture image');
  const imageId = mustDocker(
    docker(['image', 'inspect', tag, '--format', '{{.Id}}']),
    'inspect managed A&D fixture image',
  ).stdout.trim();
  requireCondition(/^sha256:[a-f0-9]{64}$/.test(imageId), 'managed A&D fixture image is not immutable');
  state.fixtureImage = { tag, imageId, removeAfter: !preexisting };
  saveRecovery();
  return imageId;
}

function verifyAdRuntime(row) {
  const runtime = inspectContainer(row.containerId);
  requireCondition(runtime, `managed A&D runtime ${row.containerId} is missing`);
  validateManagedRuntimeOwnership(runtime, row.containerId, appDockerScope);
  const serviceNetwork = runtime.NetworkSettings?.Networks?.[AD_NET];
  requireCondition(
    serviceNetwork?.IPAddress === row.host && Number(row.port) === 8080,
    `managed A&D service ${row.id} endpoint does not match its isolated runtime`,
  );
  return row.containerId;
}

function removeOwnedAdFixtureContainers() {
  if (!state.gameId || !state.fixtureImage) return;
  const durableRuntimeIds = jsonRows(
    `SELECT container_id AS id FROM "AdTeamServices" ` +
      `WHERE game_id=${state.gameId} AND container_id IS NOT NULL`,
    'managed A&D cleanup runtimes',
  ).map(({ id }) => String(id));
  const runtimeIds = [...new Set([...state.adRuntimeIds, ...durableRuntimeIds])];
  state.adRuntimeIds = runtimeIds;
  state.runtimeIds.push(...runtimeIds);
  saveRecovery();

  for (const runtimeId of new Set(runtimeIds)) {
    const runtime = inspectContainer(runtimeId);
    if (!runtime) continue;
    requireCondition(
      runtime.Image === state.fixtureImage.imageId &&
        runtime.Config?.Labels?.['rsctf.load.fixture'] === 'managed-ad-v1',
      `refusing to remove changed or unowned managed A&D runtime ${runtimeId}`,
    );
    validateManagedRuntimeOwnership(runtime, runtimeId, appDockerScope);
    mustDocker(
      docker(['rm', '-f', runtimeId]),
      `remove managed A&D fixture runtime ${runtimeId}`,
    );
  }
}

function adServiceRows() {
  return jsonRows(
    `SELECT id,"challenge_id" AS "challengeId","participation_id" AS "participationId",` +
      `host,port,container_id AS "containerId" ` +
      `FROM "AdTeamServices" WHERE game_id=${state.gameId} ORDER BY challenge_id,participation_id`,
    'A&D service matrix',
  );
}

function liveFlagRows() {
  return jsonRows(
    `SELECT flag.id,service.challenge_id AS "challengeId",` +
      `service.participation_id AS "participationId",flag.flag ` +
      `FROM "AdFlags" flag JOIN "AdTeamServices" service ON service.id=flag.team_service_id ` +
      `JOIN "AdRounds" round ON round.id=flag.round_id ` +
      `WHERE round.game_id=${state.gameId} AND NOT round.finalized ` +
      `ORDER BY service.challenge_id,service.participation_id`,
    'live A&D flag matrix',
  );
}

function roundCardinalitySnapshot() {
  const raw = sql(
    `WITH live AS (` +
      `SELECT id,number FROM "AdRounds" WHERE game_id=${state.gameId} ` +
      `ORDER BY number DESC LIMIT 1` +
    `) SELECT json_build_object(` +
      `'roundId',live.id,'roundNumber',live.number,` +
      `'flagCount',(SELECT count(*) FROM "AdFlags" flag WHERE flag.round_id=live.id),` +
      `'flagServiceCount',(SELECT count(DISTINCT flag.team_service_id) FROM "AdFlags" flag WHERE flag.round_id=live.id),` +
      `'checkCount',(SELECT count(*) FROM "AdCheckResults" result WHERE result.round_id=live.id),` +
      `'checkServiceCount',(SELECT count(DISTINCT result.team_service_id) FROM "AdCheckResults" result WHERE result.round_id=live.id),` +
      `'deliveryCount',(SELECT count(*) FROM "AdFlagDeliveryResults" result WHERE result.round_id=live.id),` +
      `'deliveryServiceCount',(SELECT count(DISTINCT result.team_service_id) FROM "AdFlagDeliveryResults" result WHERE result.round_id=live.id),` +
      `'kothResultCount',(SELECT count(*) FROM "KothControlResults" result WHERE result.game_id=${state.gameId} AND result.ad_round_id=live.id),` +
      `'kothResultHillCount',(SELECT count(DISTINCT result.challenge_id) FROM "KothControlResults" result WHERE result.game_id=${state.gameId} AND result.ad_round_id=live.id),` +
      `'cycleCount',(SELECT count(*) FROM "KothCrownCycles" cycle WHERE cycle.game_id=${state.gameId}),` +
      `'activeCycleCount',(SELECT count(*) FROM "KothCrownCycles" cycle WHERE cycle.game_id=${state.gameId} AND cycle.phase='Active'),` +
      `'duplicateFlags',(SELECT count(*) FROM (SELECT 1 FROM "AdFlags" flag WHERE flag.round_id=live.id GROUP BY flag.round_id,flag.team_service_id HAVING count(*)>1) duplicate),` +
      `'duplicateChecks',(SELECT count(*) FROM (SELECT 1 FROM "AdCheckResults" result WHERE result.round_id=live.id GROUP BY result.round_id,result.team_service_id HAVING count(*)>1) duplicate),` +
      `'duplicateDeliveries',(SELECT count(*) FROM (SELECT 1 FROM "AdFlagDeliveryResults" result WHERE result.round_id=live.id GROUP BY result.round_id,result.team_service_id HAVING count(*)>1) duplicate),` +
      `'duplicateKothResults',(SELECT count(*) FROM (SELECT 1 FROM "KothControlResults" result WHERE result.game_id=${state.gameId} GROUP BY result.game_id,result.challenge_id,result.ad_round_id HAVING count(*)>1) duplicate),` +
      `'duplicateCycles',(SELECT count(*) FROM (SELECT 1 FROM "KothCrownCycles" cycle WHERE cycle.game_id=${state.gameId} GROUP BY cycle.game_id,cycle.challenge_id,cycle.cycle_number HAVING count(*)>1) duplicate),` +
      `'duplicateActiveCycles',(SELECT count(*) FROM (SELECT 1 FROM "KothCrownCycles" cycle WHERE cycle.game_id=${state.gameId} AND cycle.phase='Active' GROUP BY cycle.game_id,cycle.challenge_id HAVING count(*)>1) duplicate)` +
      `)::text FROM live`,
  );
  requireCondition(raw, 'no authoritative round exists for cardinality proof');
  return parseJson(raw, 'round cardinality');
}

async function waitForExactRoundCardinality(timeoutSeconds = 30) {
  let snapshot;
  let lastError;
  for (let waited = 0; waited <= timeoutSeconds; waited += 1) {
    snapshot = roundCardinalitySnapshot();
    try {
      return { snapshot, proof: validateRoundCardinality(snapshot) };
    } catch (error) {
      lastError = error;
    }
    if (waited < timeoutSeconds) await sleep(1_000);
  }
  throw new Error(`round cardinality did not settle within ${timeoutSeconds}s: ${lastError?.message}; ${JSON.stringify(snapshot)}`);
}

function capabilityRows() {
  return jsonRows(
    `SELECT token.id,token.challenge_id AS "challengeId",` +
      `token.participation_id AS "participationId",token.token,` +
      `token.challenge_id AS "tokenChallengeId",target.challenge_id AS "targetChallengeId",` +
      `cycle.challenge_id AS "cycleChallengeId",token.target_id AS "targetId",token.cycle_id AS "cycleId" ` +
      `FROM "KothTokens" token ` +
      `JOIN "KothTargets" target ON target.id=token.target_id ` +
      `JOIN "KothCrownCycles" cycle ON cycle.id=token.cycle_id ` +
      `WHERE cycle.game_id=${state.gameId} AND cycle.phase='Active' ` +
      `AND token.reset_attempt=cycle.reset_attempt AND token.revoked_at IS NULL ` +
      `ORDER BY token.challenge_id,token.participation_id`,
    'KotH capability matrix',
  );
}

function latestCycle(challengeId) {
  const raw = sql(
    `SELECT json_build_object(` +
      `'id',cycle.id,'challengeId',cycle.challenge_id,'cycleNumber',cycle.cycle_number,` +
      `'phase',cycle.phase,'resetAttempt',cycle.reset_attempt,` +
      `'readinessFailures',cycle.readiness_failures,` +
      `'replacementContainerId',cycle.replacement_container_id,` +
      `'targetContainerId',target.container_id,` +
      `'updatedAtMs',(extract(epoch FROM cycle.updated_at)*1000)::bigint` +
      `)::text FROM "KothCrownCycles" cycle ` +
      `JOIN "KothTargets" target ON target.game_id=cycle.game_id AND target.challenge_id=cycle.challenge_id ` +
      `WHERE cycle.game_id=${state.gameId} AND cycle.challenge_id=${Number(challengeId)} ` +
      `ORDER BY cycle.cycle_number DESC LIMIT 1`,
  );
  requireCondition(raw, `KotH challenge ${challengeId} has no crown cycle`);
  return parseJson(raw, `KotH cycle ${challengeId}`);
}

async function waitForCrossHillResult(destinationCycle, sourceToken, baselineId) {
  const destinationChallengeId = Number(destinationCycle.challengeId);
  const destinationCycleId = Number(destinationCycle.cycleId);
  const destinationResetAttempt = Number(destinationCycle.resetAttempt);
  const destinationContainerId = String(destinationCycle.containerId || '');
  let observed = null;
  for (let waited = 0; waited <= markerTimeoutSeconds; waited += 1) {
    const raw = sql(
      `SELECT json_build_object(` +
        `'id',result.id,'gameId',result.game_id,'challengeId',result.challenge_id,` +
        `'cycleId',result.cycle_id,'containerId',result.container_id,` +
        `'tokenWindowAttempt',result.token_window_attempt,` +
        `'adRoundId',result.ad_round_id,'roundGameId',round.game_id,` +
        `'roundNumber',round.number,'plannedStartRound',cycle.planned_start_round,` +
        `'plannedEndRound',cycle.planned_end_round,` +
        `'markerObserved',result.marker_observed,'status',result.status,` +
        `'isScorable',result.is_scorable,'tokenId',result.token_id,` +
        `'controller',result.controlling_participation_id,` +
        `'responsible',result.responsible_participation_id,` +
        `'sourceTokenMatches',(SELECT COALESCE(max(token.id),0) FROM "KothTokens" token ` +
          `WHERE token.id=${sourceToken.id} AND token.challenge_id=${sourceToken.challengeId} ` +
          `AND token.token=${sqlLiteral(sourceToken.token)}),` +
        `'destinationMatches',(SELECT count(*) FROM "KothTokens" token ` +
          `WHERE token.challenge_id=${Number(destinationChallengeId)} ` +
          `AND token.token=${sqlLiteral(sourceToken.token)})` +
        `)::text FROM "KothControlResults" result ` +
        `JOIN "KothCrownCycles" cycle ON cycle.id=result.cycle_id ` +
          `AND cycle.game_id=result.game_id AND cycle.challenge_id=result.challenge_id ` +
        `JOIN "AdRounds" round ON round.id=result.ad_round_id AND round.game_id=result.game_id ` +
        `WHERE result.game_id=${state.gameId} AND result.challenge_id=${Number(destinationChallengeId)} ` +
        `AND result.cycle_id=${destinationCycleId} ` +
        `AND result.container_id=${sqlLiteral(destinationContainerId)} ` +
        `AND result.token_window_attempt=${destinationResetAttempt} ` +
        `AND round.number BETWEEN cycle.planned_start_round AND cycle.planned_end_round ` +
        `AND result.id>${Number(baselineId)} AND result.marker_observed ` +
        `ORDER BY result.id LIMIT 1`,
    );
    if (raw) {
      observed = parseJson(raw, 'cross-hill control evidence');
      break;
    }
    if (waited < markerTimeoutSeconds) await sleep(1_000);
  }
  requireCondition(observed, `cross-hill marker was not sampled within ${markerTimeoutSeconds}s`);
  return observed;
}

async function prepareFixture() {
  const now = A.nowMs();
  state.gameId = await A.createGame({
    title,
    hidden: false,
    practiceMode: false,
    acceptWithoutReview: true,
    start: now - 3_600_000,
    end: now + 3_600_000,
    adTickSeconds: 30,
    adFlagLifetimeTicks: 5,
    adGetflagWindowFraction: 0.9,
    adMinGracePeriodSeconds: 1,
    adResetCooldownMinutes: 5,
    kothEpochTicks: 24,
    kothCycleTicks: 12,
    kothChampionCooldownTicks: 1,
    kothClaimConfirmationTicks: 2,
  });
  saveRecovery();
  await A.setAdScoringPaused(state.gameId, true);
  sql(
    `UPDATE "Games" SET ad_warmup_seconds=0,ad_tick_seconds=30,ad_flag_lifetime_ticks=5,` +
      `ad_getflag_window_fraction=0.9,ad_min_grace_period_seconds=1,` +
      `koth_epoch_ticks=24,koth_cycle_ticks=12,koth_champion_cooldown_ticks=1,` +
      `koth_claim_confirmation_ticks=2 WHERE id=${state.gameId} AND title=${sqlLiteral(title)}`,
  );

  const adImage = buildManagedAdImage();
  for (let index = 0; index < AD_CHALLENGE_COUNT; index += 1) {
    const challengeId = await A.createChallenge(state.gameId, {
      title: `${runId}-ad-${index + 1}`,
      category: 'Pwn',
      type: 'AttackDefense',
    });
    const checker = A.prepareExactChecker(state.gameId, challengeId);
    await A.setChallenge(state.gameId, challengeId, {
      containerImage: adImage,
      memoryLimit: 64,
      cpuCount: 1,
      exposePort: 8080,
      adSelfHosted: false,
      adAllowSelfReset: true,
      adAllowEgress: false,
      adCheckerImage: checker,
      flagTemplate: `flag{multi_${index + 1}_[TEAM_HASH]_[GUID]}`,
    });
    await A.rebuildChallengeImage(state.gameId, challengeId, adImage, `A&D service ${index + 1}`);
    await A.addFlags(state.gameId, challengeId, [`flag{${runId}_ad_placeholder_${index + 1}}`]);
    await A.setChallenge(state.gameId, challengeId, { isEnabled: true });
    state.adChallengeIds.push(challengeId);
    state.challengeIds.push(challengeId);
    saveRecovery();
  }

  const kothImage = kothOverride?.image ?? A.buildCompetitiveKothImage();
  const kothPort = kothOverride?.port ?? 8080;
  for (let index = 0; index < KOTH_CHALLENGE_COUNT; index += 1) {
    const challengeId = await A.createChallenge(state.gameId, {
      title: `${runId}-hill-${index + 1}`,
      category: 'Pwn',
      type: 'KingOfTheHill',
    });
    const checker = A.prepareKothChecker(state.gameId, challengeId);
    await A.setChallenge(state.gameId, challengeId, {
      containerImage: kothImage,
      memoryLimit: 64,
      cpuCount: 1,
      exposePort: kothPort,
      adAllowEgress: false,
      adCheckerImage: checker,
    });
    await A.rebuildChallengeImage(state.gameId, challengeId, kothImage, `KotH hill ${index + 1}`);
    await A.addFlags(state.gameId, challengeId, [`flag{${runId}_koth_placeholder_${index + 1}}`]);
    await A.setChallenge(state.gameId, challengeId, { isEnabled: true });
    state.kothChallengeIds.push(challengeId);
    state.challengeIds.push(challengeId);
    saveRecovery();
  }

  const cohort = A.seedCohort(state.gameId, TEAM_COUNT);
  state.userIds = cohort.userIds;
  state.teamIds = cohort.teamIds;
  state.participationIds = cohort.partIds;
  saveRecovery();

  const ensured = await A.api('POST', `/api/edit/games/${state.gameId}/ad/EnsureContainers`, {
    jwt: A.adminJwt(),
    ip: '10.251.8.1',
    timeoutMs: 180_000,
  });
  requireCondition(ensured.status === 200, `multi-domain provisioning returned ${ensured.status}: ${ensured.text?.slice(0, 300)}`);
  let services = [];
  let targets = [];
  for (let waited = 0; waited <= 60; waited += 1) {
    services = adServiceRows();
    targets = jsonRows(
      `SELECT id,challenge_id AS "challengeId",container_id AS "containerId" ` +
        `FROM "KothTargets" WHERE game_id=${state.gameId} ORDER BY challenge_id`,
      'KotH targets',
    );
    if (services.length === TEAM_COUNT * AD_CHALLENGE_COUNT &&
        services.every(({ containerId }) => /^[a-f0-9]{64}$/.test(String(containerId || ''))) &&
        targets.length === KOTH_CHALLENGE_COUNT &&
        targets.every(({ containerId }) => /^[a-f0-9]{64}$/.test(String(containerId || '')))) break;
    if (waited < 60) await sleep(1_000);
  }
  state.serviceIds = services.map(({ id }) => Number(id));
  state.evidence.serviceMatrix = validateAdServiceMatrix(
    services,
    state.adChallengeIds,
    state.participationIds,
  );
  const managedAdRuntimeIds = services.map(verifyAdRuntime);
  requireCondition(new Set(managedAdRuntimeIds).size === 4, 'managed A&D runtime identities are not one per service');
  state.adRuntimeIds = managedAdRuntimeIds;
  state.runtimeIds.push(...managedAdRuntimeIds);
  state.targetIds = targets.map(({ id }) => Number(id));
  requireCondition(state.targetIds.length === KOTH_CHALLENGE_COUNT, 'fixture did not create two exact KotH targets');
  const managedHills = state.kothChallengeIds.map((challengeId) =>
    discoverManagedKothHill(state.gameId, challengeId));
  requireCondition(
    new Set(managedHills.map(({ backendId }) => backendId)).size === KOTH_CHALLENGE_COUNT,
    'KotH challenges share a managed runtime identity',
  );
  state.runtimeIds.push(...managedHills.map(({ backendId }) => backendId));
  saveRecovery();

  await A.setAdScoringPaused(state.gameId, false);
  const epoch = await A.waitForEpochReady(state.gameId, TEAM_COUNT);
  const crowns = [];
  for (const challengeId of state.kothChallengeIds) {
    crowns.push(await A.waitForCrownReady(state.gameId, challengeId, TEAM_COUNT));
  }
  state.cycleIds = crowns.map(({ cycleId }) => Number(cycleId));
  state.runtimeIds.push(...crowns.map(({ containerId }) => containerId));
  state.evidence.readiness = {
    startRound: epoch.startRound,
    liveRound: epoch.liveRound,
    services: epoch.rosterServices,
    verifiedFlags: epoch.verifiedFlags,
    hills: crowns.map(({ cycleId, cycleNumber, phase, tokenCount, containerId }) => ({
      cycleId,
      cycleNumber,
      phase,
      tokenCount,
      containerId,
    })),
  };
  const cardinality = await waitForExactRoundCardinality();
  state.evidence.roundCardinality = {
    roundId: cardinality.snapshot.roundId,
    roundNumber: cardinality.snapshot.roundNumber,
    ...cardinality.proof,
    flags: cardinality.snapshot.flagCount,
    checks: cardinality.snapshot.checkCount,
    deliveries: cardinality.snapshot.deliveryCount,
    kothResults: cardinality.snapshot.kothResultCount,
    cycles: cardinality.snapshot.cycleCount,
  };
  const scoreboardResponse = await A.api('GET', `/api/Game/${state.gameId}/Ad/Scoreboard`, {
    jwt: A.adminJwt(),
    ip: '10.251.9.1',
    baseUrl: webTargets[0],
  });
  requireCondition(
    scoreboardResponse.status === 200,
    `two-service scoreboard returned ${scoreboardResponse.status}: ${scoreboardResponse.text?.slice(0, 300)}`,
  );
  state.evidence.scoreboard = validateAdScoreboardReconciliation(
    unwrap(scoreboardResponse),
    state.adChallengeIds,
    state.participationIds,
  );
  saveRecovery();
}

async function verifyDomainIsolation() {
  const flags = liveFlagRows();
  state.evidence.flagMatrix = validateFlagMatrix(flags, state.adChallengeIds, state.participationIds);

  const attackerId = state.participationIds[0];
  const victimId = state.participationIds[1];
  const victimFlags = flags.filter(({ participationId }) => Number(participationId) === victimId);
  const ownFlags = flags.filter(({ participationId }) => Number(participationId) === attackerId);
  requireCondition(victimFlags.length === 2 && ownFlags.length === 2, 'live flag roster is not a complete 2×2 matrix');
  const attackBaseline = Number(sql(`SELECT COALESCE(max(id),0) FROM "AdAttacks"`));
  const attackerStamp = sql(`SELECT security_stamp FROM "AspNetUsers" WHERE id=${sqlLiteral(state.userIds[0])}::uuid`);
  const attackerJwt = mintJwt(state.userIds[0], attackerStamp, 1);
  const accepted = await A.api('POST', `/api/Game/${state.gameId}/Ad/Submit`, {
    jwt: attackerJwt,
    ip: '10.251.10.1',
    body: { flags: victimFlags.map(({ flag }) => flag) },
  });
  const acceptedModel = unwrap(accepted);
  requireCondition(
    accepted.status === 200 && acceptedModel?.acceptedCount === 2 &&
      acceptedModel.results?.every(({ status }) => status === 'accepted'),
    `two-service A&D capture failed: ${accepted.status} ${accepted.text?.slice(0, 300)}`,
  );
  const attacks = jsonRows(
    `SELECT attack.id,service.challenge_id AS "serviceChallengeId",` +
      `flag_service.challenge_id AS "flagChallengeId",` +
      `service.participation_id AS "victimParticipationId" ` +
      `FROM "AdAttacks" attack ` +
      `JOIN "AdTeamServices" service ON service.id=attack.victim_team_service_id ` +
      `JOIN "AdFlags" flag ON flag.id=attack.flag_id ` +
      `JOIN "AdTeamServices" flag_service ON flag_service.id=flag.team_service_id ` +
      `WHERE attack.id>${attackBaseline} AND attack.attacker_participation_id=${attackerId} ` +
      `ORDER BY service.challenge_id`,
    'accepted A&D attack attribution',
  );
  state.evidence.attackAttribution = validateAcceptedAttackAttribution(
    attacks,
    state.adChallengeIds,
    victimId,
  );

  const selfAttackBaseline = Number(sql(`SELECT count(*) FROM "AdAttacks" WHERE attacker_participation_id=${attackerId}`));
  const rejected = await A.api('POST', `/api/Game/${state.gameId}/Ad/Submit`, {
    jwt: attackerJwt,
    ip: '10.251.10.2',
    body: { flags: ownFlags.map(({ flag }) => flag) },
  });
  const rejectedModel = unwrap(rejected);
  requireCondition(
    rejected.status === 200 && rejectedModel?.acceptedCount === 0 &&
      rejectedModel.results?.length === 2 &&
      rejectedModel.results.every(({ status }) => status === 'self_attack'),
    `cross-challenge self-flag rejection failed: ${rejected.status} ${rejected.text?.slice(0, 300)}`,
  );
  requireCondition(
    Number(sql(`SELECT count(*) FROM "AdAttacks" WHERE attacker_participation_id=${attackerId}`)) === selfAttackBaseline,
    'rejected cross-challenge self flags created attack rows',
  );
  state.evidence.selfFlagRejection = { rejected: 2, inserted: 0 };

  const capabilities = capabilityRows();
  state.evidence.capabilityMatrix = validateKothCapabilityMatrix(
    capabilities,
    state.kothChallengeIds,
    state.participationIds,
  );
  const playerCapabilities = capabilities.filter(({ participationId }) => Number(participationId) === attackerId);
  requireCondition(playerCapabilities.length === 2, 'player did not receive one capability per hill');
  for (let index = 0; index < state.kothChallengeIds.length; index += 1) {
    const challengeId = state.kothChallengeIds[index];
    const response = await A.api('GET', `/api/game/${state.gameId}/ad/koth/${challengeId}/token`, {
      jwt: attackerJwt,
      ip: `10.251.11.${index + 1}`,
      baseUrl: webTargets[index % webTargets.length],
    });
    const model = unwrap(response);
    const durable = playerCapabilities.find((capability) => Number(capability.challengeId) === challengeId);
    requireCondition(
      response.status === 200 && model?.status === 'ready' && model?.token === durable?.token,
      `replica ${index + 1} returned a mismatched hill capability`,
    );
  }

  const [source, destination] = playerCapabilities.sort((left, right) =>
    Number(left.challengeId) - Number(right.challengeId));
  const destinationCycle = {
    ...A.crownReadiness(state.gameId, destination.challengeId),
    challengeId: Number(destination.challengeId),
  };
  const baselineResultId = Number(sql(`SELECT COALESCE(max(id),0) FROM "KothControlResults"`));
  A.kothCaptureWrite(destinationCycle.containerId, source.token);
  const crossHill = await waitForCrossHillResult(destinationCycle, source, baselineResultId);
  state.evidence.crossHillRejection = validateCrossHillRejection(
    crossHill,
    {
      sourceTokenId: source.id,
      sourceChallengeId: source.challengeId,
      destinationChallengeId: destination.challengeId,
      gameId: state.gameId,
      cycleId: destinationCycle.cycleId,
      containerId: destinationCycle.containerId,
      resetAttempt: destinationCycle.resetAttempt,
    },
  );
  const markerContainer = inspectContainer(crossHill.containerId);
  if (markerContainer) {
    mustDocker(docker(['exec', crossHill.containerId, 'rm', '-f', '/koth/king']), 'clear cross-hill marker');
  }
  saveRecovery();
}

async function verifyFailIsolatedHillRecovery() {
  await A.setAdScoringPaused(state.gameId, true);
  await sleep(1_000);
  const [lowerId, higherId] = [...state.kothChallengeIds].sort((left, right) => left - right);
  const lowerBefore = latestCycle(lowerId);
  const higherBefore = latestCycle(higherId);
  requireCondition(
    lowerBefore.phase === 'Active' && higherBefore.phase === 'Active' &&
      lowerBefore.replacementContainerId === lowerBefore.targetContainerId &&
      higherBefore.replacementContainerId === higherBefore.targetContainerId,
    'both exact hills must be Active before fault injection',
  );
  originalLowerChecker = sql(
    `SELECT ad_checker_image FROM "GameChallenges" ` +
      `WHERE game_id=${state.gameId} AND id=${lowerId} AND title=${sqlLiteral(`${runId}-hill-1`)}`,
  );
  requireCondition(originalLowerChecker, 'lower hill checker ownership proof failed');
  const missingChecker = `/data/files/checkers/load/${state.gameId}/missing-${randomUUID()}`;
  const checkerUpdated = sql(
    `UPDATE "GameChallenges" SET ad_checker_image=${sqlLiteral(missingChecker)} ` +
      `WHERE game_id=${state.gameId} AND id=${lowerId} ` +
      `AND ad_checker_image=${sqlLiteral(originalLowerChecker)} RETURNING id`,
  );
  requireCondition(Number(checkerUpdated) === lowerId, 'lower hill checker fault CAS failed');
  const faultMarker = `multi-domain-readiness-${runId}`;
  const faultedIds = String(sql(
    `UPDATE "KothCrownCycles" cycle SET phase='ReadinessPending',` +
      `readiness_error=${sqlLiteral(faultMarker)},last_error=${sqlLiteral(faultMarker)},` +
      `updated_at=clock_timestamp() FROM "KothTargets" target ` +
      `WHERE cycle.id IN (${lowerBefore.id},${higherBefore.id}) AND cycle.phase='Active' ` +
      `AND target.game_id=cycle.game_id AND target.challenge_id=cycle.challenge_id ` +
      `AND target.container_id=cycle.replacement_container_id RETURNING cycle.id`,
  ) || '').split('\n').map(Number).sort((left, right) => left - right);
  requireCondition(
    faultedIds.length === 2 && faultedIds[0] === Math.min(lowerBefore.id, higherBefore.id) &&
      faultedIds[1] === Math.max(lowerBefore.id, higherBefore.id),
    'fault injection did not fence both exact crown cycles',
  );

  const failed = await A.api(
    'POST',
    `/api/edit/games/${state.gameId}/ad/koth/${lowerId}/recover`,
    { jwt: A.adminJwt(), ip: '10.251.12.1', timeoutMs: 120_000 },
  );
  const lowerAfter = latestCycle(lowerId);
  const higherAfter = latestCycle(higherId);

  const checkerRestored = sql(
    `UPDATE "GameChallenges" SET ad_checker_image=${sqlLiteral(originalLowerChecker)} ` +
      `WHERE game_id=${state.gameId} AND id=${lowerId} ` +
      `AND ad_checker_image=${sqlLiteral(missingChecker)} RETURNING id`,
  );
  requireCondition(Number(checkerRestored) === lowerId, 'lower hill checker restore CAS failed');
  originalLowerChecker = null;
  const recoveredResponse = await A.api(
    'POST',
    `/api/stateful/edit/games/${state.gameId}/ad/koth/${lowerId}/recover`,
    { jwt: A.adminJwt(), ip: '10.251.12.2', timeoutMs: 120_000 },
  );
  requireCondition(
    recoveredResponse.status === 200 && unwrap(recoveredResponse)?.challengeId === lowerId,
    `lower hill recovery failed: ${recoveredResponse.status} ${recoveredResponse.text?.slice(0, 300)}`,
  );
  const lowerRecovered = latestCycle(lowerId);
  state.evidence.failIsolation = validateFaultIsolation({
    lowerBefore,
    higherBefore,
    failedResponseStatus: failed.status,
    lowerAfter,
    higherAfter,
    lowerRecovered,
  });
  state.evidence.failIsolation = {
    ...state.evidence.failIsolation,
    lowerChallengeId: lowerId,
    higherChallengeId: higherId,
    controlledFailureStatus: failed.status,
    controlledFailurePath: 'legacy',
    successfulRecoveryPath: 'stateful',
    higherPhaseWhileLowerFailed: higherAfter.phase,
    lowerFinalPhase: lowerRecovered.phase,
  };
  saveRecovery();
}

function checkerDirectoryCount() {
  if (!state.gameId) return 0;
  let present = 0;
  for (const container of serverContainers) {
    const path = `/data/files/checkers/load/${state.gameId}`;
    const absent = docker(['exec', container, 'test', '!', '-e', path]);
    if (absent.status === 0) continue;
    const exists = docker(['exec', container, 'test', '-e', path]);
    if (exists.status !== 0) throw new Error(`cannot audit checker path ${path} in ${container}`);
    present += 1;
  }
  return present;
}

function cleanupSnapshot() {
  const gameId = Number(state.gameId);
  const challengeIds = state.challengeIds.filter(Number.isSafeInteger);
  const userIds = state.userIds.filter(Boolean);
  const teamIds = state.teamIds.filter(Number.isSafeInteger);
  const participationIds = state.participationIds.filter(Number.isSafeInteger);
  const serviceIds = state.serviceIds.filter(Number.isSafeInteger);
  const targetIds = state.targetIds.filter(Number.isSafeInteger);
  const cycleIds = state.cycleIds.filter(Number.isSafeInteger);
  const count = (table, column, values, cast = '') => values.length
    ? Number(sql(`SELECT count(*) FROM "${table}" WHERE ${column} IN (` +
      values.map((value) => `${sqlLiteral(value)}${cast}`).join(',') + ')'))
    : 0;
  const exactRows =
    count('Games', 'id', gameId ? [gameId] : []) +
    count('GameChallenges', 'game_id', gameId ? [gameId] : []) +
    count('GameEvents', 'game_id', gameId ? [gameId] : []) +
    count('GameNotices', 'game_id', gameId ? [gameId] : []) +
    count('GameManagers', 'game_id', gameId ? [gameId] : []) +
    count('BuildRecords', 'game_id', gameId ? [gameId] : []) +
    count('Divisions', 'game_id', gameId ? [gameId] : []) +
    count('DivisionChallengeConfigs', 'challenge_id', challengeIds) +
    count('FlagContexts', 'challenge_id', challengeIds) +
    count('AspNetUsers', 'id', userIds, '::uuid') +
    count('Teams', 'id', teamIds) +
    count('Participations', 'id', participationIds) +
    count('UserParticipations', 'participation_id', participationIds) +
    count('TeamMembers', 'team_id', teamIds) +
    count('AdTeamApiTokens', 'participation_id', participationIds) +
    count('AdSshKeys', 'participation_id', participationIds) +
    count('AdVpnPeers', 'participation_id', participationIds) +
    count('AdTeamServices', 'game_id', gameId ? [gameId] : []) +
    count('Containers', 'ad_team_service_id', serviceIds) +
    count('AdFlags', 'team_service_id', serviceIds) +
    count('AdCheckResults', 'team_service_id', serviceIds) +
    count('AdFlagDeliveryResults', 'team_service_id', serviceIds) +
    count('AdAttacks', 'attacker_participation_id', participationIds) +
    count('KothTargets', 'game_id', gameId ? [gameId] : []) +
    count('KothCrownCycles', 'game_id', gameId ? [gameId] : []) +
    count('KothTokens', 'participation_id', participationIds) +
    count('KothControlResults', 'game_id', gameId ? [gameId] : []) +
    count('KothOfficialConfigs', 'game_id', gameId ? [gameId] : []) +
    count('KothClaimStates', 'target_id', targetIds) +
    count('KothAcquisitions', 'cycle_id', cycleIds) +
    count('KothCycleCooldowns', 'cycle_id', cycleIds) +
    count('KothCycleAuditReceipts', 'cycle_id', cycleIds) +
    count('AdRounds', 'game_id', gameId ? [gameId] : []) +
    count('AdEpochRollups', 'game_id', gameId ? [gameId] : []) +
    count('AdEpochServiceRollups', 'game_id', gameId ? [gameId] : []) +
    count('AdEpochTeamRollups', 'game_id', gameId ? [gameId] : []) +
    count('KothEpochRollups', 'game_id', gameId ? [gameId] : []) +
    count('KothEpochTeamRollups', 'game_id', gameId ? [gameId] : []) +
    count('KothEpochHillRollups', 'game_id', gameId ? [gameId] : []) +
    count('SuspicionEvents', 'game_id', gameId ? [gameId] : []) +
    count('HoneypotHits', 'game_id', gameId ? [gameId] : []);
  const runtimeContainers = [...new Set(state.runtimeIds.filter(Boolean))]
    .reduce((sum, id) => sum + Number(inspectContainer(id) !== null), 0);
  return Object.freeze({
    games: count('Games', 'id', gameId ? [gameId] : []),
    exactDatabaseRows: exactRows,
    runtimeContainers,
    fixtureImages: Number(Boolean(state.fixtureImage?.removeAfter && docker(['image', 'inspect', state.fixtureImage.tag]).status === 0)),
    checkerDirectories: checkerDirectoryCount(),
  });
}

async function cleanup() {
  const errors = [];
  const attempt = async (label, action) => {
    try { await action(); } catch (error) { errors.push(`${label}: ${error.message}`); }
  };
  if (state.gameId && Number(sql(`SELECT count(*) FROM "Games" WHERE id=${state.gameId}`)) > 0) {
    await attempt('restore lower checker', async () => {
      if (!originalLowerChecker) return;
      sql(
        `UPDATE "GameChallenges" SET ad_checker_image=${sqlLiteral(originalLowerChecker)} ` +
          `WHERE game_id=${state.gameId} AND id=${Math.min(...state.kothChallengeIds)}`,
      );
      originalLowerChecker = null;
    });
    await attempt('pause scoring', () => A.setAdScoringPaused(state.gameId, true));
    await attempt('reconcile exact database identities', async () => {
      const ids = (query, label) => jsonRows(query, label).map(({ id }) => Number(id));
      state.serviceIds = [...new Set([
        ...state.serviceIds,
        ...ids(`SELECT id FROM "AdTeamServices" WHERE game_id=${state.gameId}`, 'cleanup A&D services'),
      ])];
      state.targetIds = [...new Set([
        ...state.targetIds,
        ...ids(`SELECT id FROM "KothTargets" WHERE game_id=${state.gameId}`, 'cleanup KotH targets'),
      ])];
      state.cycleIds = [...new Set([
        ...state.cycleIds,
        ...ids(`SELECT id FROM "KothCrownCycles" WHERE game_id=${state.gameId}`, 'cleanup crown cycles'),
      ])];
      saveRecovery();
    });
    await attempt('capture managed KotH identities', async () => {
      state.runtimeIds.push(...A.kothContainerIdsForGames([state.gameId]));
      saveRecovery();
    });
    await attempt('event namespace', async () => {
      try {
        await A.teardownNamespace([state.gameId]);
      } catch (error) {
        removeOwnedAdFixtureContainers();
        deleteDisposableLoadGame(state.gameId, title, { runtimeIds: state.runtimeIds });
        state.evidence.historyRetentionCleanup =
          'public hard-delete preserved started evidence; exact disposable graph fallback completed';
        saveRecovery();
      }
    });
  }
  await attempt('managed A&D fixture image', async () => {
    if (!state.fixtureImage?.removeAfter) return;
    const inspected = docker(['image', 'inspect', state.fixtureImage.tag]);
    if (inspected.status !== 0) return;
    const [image] = parseJson(inspected.stdout, 'managed A&D fixture image cleanup');
    requireCondition(
      image?.Id === state.fixtureImage.imageId && image?.Config?.Labels?.['rsctf.load.fixture'] === 'managed-ad-v1',
      'refusing to remove a changed or unowned managed A&D fixture image',
    );
    mustDocker(docker(['image', 'rm', state.fixtureImage.tag]), 'remove managed A&D fixture image tag');
  });
  const passes = [];
  for (let index = 0; index < 2; index += 1) {
    await sleep(cleanupStabilityMs);
    await attempt(`cleanup snapshot ${index + 1}`, async () => passes.push(cleanupSnapshot()));
  }
  if (passes.length === 2) {
    await attempt('stable cleanup proof', async () => validateCleanupPasses(passes));
  }
  state.cleanup = { delayMs: cleanupStabilityMs, passes };
  saveRecovery();
  if (errors.length) throw new Error(`multi-domain cleanup failed:\n- ${errors.join('\n- ')}`);
}

async function main() {
  // These are deliberately the first Docker/network/database interactions.
  const safeOrigins = assertSafeAdminTarget(process.env);
  assertDisposableEditStack({ webTargets, controlTarget, serverContainers });
  processLock = await acquireExclusiveProcessLock(loadOrchestrationLockPath, {
    label: 'multi-domain acceptance',
    metadata: { target: TARGET, runId },
  });
  databaseLock = await acquireAdminLifecycleDatabaseLock();
  state.runtimeIdentity = { before: inspectUniformServerRuntimeIdentity(serverContainers), after: null };
  appDockerScope = discoverAppDockerScope();
  saveRecovery();

  let failure;
  try {
    requireCondition(
      safeOrigins.webTargets.length === webTargets.length && safeOrigins.controlTarget === controlTarget,
      'validated origin set diverged before credential use',
    );
    await assertRuntimeRoles({ webTargets, controlTarget });
    await A.preflight();
    await prepareFixture();
    await verifyDomainIsolation();
    await verifyFailIsolatedHillRecovery();
    state.scenarioCompleted = true;
    saveRecovery();
  } catch (error) {
    failure = error;
    state.failure = String(error?.stack || error?.message || error);
    saveRecovery();
  }

  try {
    await cleanup();
  } catch (cleanupError) {
    failure = failure
      ? new AggregateError([failure, cleanupError], 'multi-domain scenario and cleanup both failed')
      : cleanupError;
    state.failure = String(failure?.stack || failure?.message || failure);
    saveRecovery();
  }

  try {
    state.runtimeIdentity.after = inspectUnchangedServerRuntimeIdentity(
      state.runtimeIdentity.before,
      serverContainers,
    );
    const logTargets = originalServerRuntimeLogTargets(state.runtimeIdentity.before);
    const fatalCounts = Object.fromEntries(logTargets
      .map(({ name, containerId }) => [name, countContainerFatalLogs(containerId, state.startedAt)]));
    requireCondition(Object.values(fatalCounts).every((count) => count === 0), `server fatal logs: ${JSON.stringify(fatalCounts)}`);
    const checkerOwnership = Object.fromEntries(logTargets.map(({ name, containerId }) => [name, {
      role: configuredRuntimeRole(containerId),
      violations: checkerOwnershipViolationCount(containerId),
    }]));
    requireCondition(
      Object.values(checkerOwnership).some(({ role }) => role === 'web') &&
        Object.values(checkerOwnership).every(({ violations }) => violations === 0),
      `checker ownership log violations: ${JSON.stringify(checkerOwnership)}`,
    );
    inspectUnchangedServerRuntimeIdentity(state.runtimeIdentity.before, serverContainers);
    state.fatalLogCounts = fatalCounts;
    state.checkerOwnershipLogAudit = checkerOwnership;
  } catch (verificationError) {
    failure = failure
      ? new AggregateError([failure, verificationError], 'multi-domain scenario and final verification both failed')
      : verificationError;
    state.failure = String(failure?.stack || failure?.message || failure);
    saveRecovery();
  }

  if (failure) throw failure;
  state.completed = true;
  state.completedAt = Date.now();
  saveRecovery();
  console.log(`✓ multi-domain acceptance passed for game ${state.gameId}`);
  console.log('  A&D: 2 challenges × 2 teams, exact flags and capture attribution');
  console.log('  KotH: 2 hills × 2 teams, cross-hill token rejected, later hill not starved');
  console.log('  cleanup: two identical zero-residue snapshots; server identities unchanged');
  console.log(`  audit manifest: ${keepManifest ? recoveryPath : 'removed after success'}`);
  if (!keepManifest) removeRecovery(recoveryPath);
}

main()
  .catch((error) => {
    console.error('multi-domain acceptance failed:', error?.stack || error?.message || error);
    console.error(`recovery manifest: ${recoveryPath}`);
    process.exitCode = 1;
  })
  .finally(async () => {
    await databaseLock?.release().catch((error) => {
      console.error(`database lock release failed: ${error.message}`);
      process.exitCode = 1;
    });
    await processLock?.release().catch((error) => {
      console.error(`process lock release failed: ${error.message}`);
      process.exitCode = 1;
    });
  });
