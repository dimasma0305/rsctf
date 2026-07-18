// Stand up two realistic events and seed hundreds of teams, then write .lifecycle-state.json.
//   npm run provision            # default 300 jeopardy + 300 A&D/KotH teams
//   TEAMS_JEO=200 TEAMS_AD=200 CH_STATIC=8 npm run provision
//   EVENT_DURATION_SECONDS=3600 npm run provision
//   npm run provision -- --down  # teardown the namespace
import * as A from './applib.mjs';
import { docker, RSCTF, sql } from './lib.mjs';
import { randomUUID } from 'node:crypto';
import { rmSync } from 'node:fs';
import {
  acquireExclusiveProcessLock,
  InFlightMutationDrain,
  loadOrchestrationLockPath,
} from './process-control.mjs';
import { teardownVpnTeamClients } from './team-clients.mjs';
import { kothContainerOverride } from './fixture-image-config.js';

function positiveIntegerEnv(name, fallback) {
  const raw = process.env[name];
  if (raw === undefined) return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value <= 0 || !Number.isSafeInteger(value * 1000)) {
    throw new Error(`${name} must be a positive integer number of seconds (got ${raw})`);
  }
  return value;
}

const TEAMS_JEO = Number(process.env.TEAMS_JEO || 300);
const TEAMS_AD = Number(process.env.TEAMS_AD || 300);
const CH_STATIC = Number(process.env.CH_STATIC || 8);
const CONTAINER_IMAGE = process.env.CONTAINER_IMAGE || 'nginx:alpine';
const EVENT_DURATION_SECONDS = positiveIntegerEnv('EVENT_DURATION_SECONDS', 7 * 86400);
const REALISTIC_COMPETITION = process.env.REALISTIC_COMPETITION === '1';
const COMPETITION_SEED = process.env.SIMULATION_SEED || 'rsctf-competitive-v2';
const KOTH_CONTAINER_OVERRIDE = kothContainerOverride(process.env);
const KOTH_CONFIG = Object.freeze({
  kothEpochTicks: 12,
  kothCycleTicks: 3,
  kothChampionCooldownTicks: 1,
  kothClaimConfirmationTicks: 2,
});

function currentAttemptGameIds(attempt) {
  const ids = new Set(
    [...(Array.isArray(attempt.gameIds) ? attempt.gameIds : []), attempt.jeoGame, attempt.mixGame]
      .map(Number)
      .filter((id) => Number.isSafeInteger(id) && id > 0)
  );
  if (!Number.isSafeInteger(attempt.createdAtMs)) return [...ids];
  const rows = sql(
    `SELECT id FROM "Games" WHERE title IN (` +
      `'LOADTEST-JEO-${attempt.createdAtMs}','LOADTEST-MIX-${attempt.createdAtMs}')`
  );
  for (const row of rows.split('\n').filter(Boolean)) ids.add(Number(row));
  return [...ids].filter((id) => Number.isSafeInteger(id) && id > 0);
}

function removeContainer(id, cleanupErrors) {
  if (!id || docker(['inspect', id]).status !== 0) return;
  const result = docker(['rm', '-f', id]);
  if (result.status !== 0 && !/no such container/i.test(result.stderr)) {
    cleanupErrors.push(`remove container ${id.slice(0, 12)}: ${result.stderr.trim()}`);
  }
}

async function cleanupFailedProvision(attempt) {
  const cleanupErrors = [];
  let gameIds = attempt.gameIds;
  try {
    gameIds = currentAttemptGameIds(attempt);
    attempt.gameIds = gameIds;
  } catch (error) {
    cleanupErrors.push(`discover provisioned games: ${error.message}`);
  }
  try {
    // Once a game exists, labels are the only authority for fleet cleanup.
    // With no committed game yet, teardownFleet can still use its in-memory
    // active scope. No fixed-name resource is removed by a raw-ID fallback.
    A.teardownFleet(gameIds.length ? { gameIds } : null);
  } catch (error) {
    cleanupErrors.push(`teardown BYOC fleet: ${error.message}`);
  }
  let kothContainers = [];
  try {
    kothContainers = A.kothContainerIdsForGames(gameIds);
  } catch (error) {
    cleanupErrors.push(`discover KotH containers: ${error.message}`);
  }

  if (gameIds.length) {
    try {
      await A.teardownNamespace(gameIds);
    } catch (error) {
      cleanupErrors.push(`teardown games ${gameIds.join(',')}: ${error.message}`);
    }
  }

  for (const id of new Set(kothContainers)) {
    removeContainer(id, cleanupErrors);
  }

  // teardownNamespace normally removes these. Repeat the exact namespaced removal
  // so a transient API/SQL cleanup failure cannot leave executable checker files.
  for (const gameId of gameIds) {
    const result = docker(['exec', RSCTF, 'rm', '-rf', `/data/files/checkers/load/${gameId}`]);
    if (result.status !== 0) {
      cleanupErrors.push(`remove checker directory for game ${gameId}: ${result.stderr.trim()}`);
    }
  }
  return cleanupErrors;
}

let activeProvision = null;
let orchestrationLock = null;
let shutdownSignal = null;
const inFlightMutations = new InFlightMutationDrain();
let resolveShutdown;
const shutdownRequested = new Promise((resolve) => {
  resolveShutdown = resolve;
});

function shutdownError(signal = shutdownSignal) {
  return new Error(`provision interrupted by ${signal}`);
}

const shutdownHandlers = Object.fromEntries(
  ['SIGINT', 'SIGTERM'].map((signal) => [
    signal,
    () => {
      if (shutdownSignal !== null) {
        process.removeListener(signal, shutdownHandlers[signal]);
        process.kill(process.pid, signal);
        return;
      }
      shutdownSignal = signal;
      resolveShutdown(signal);
      console.error(`\n  ${signal} received; stopping provision and cleaning up...`);
    },
  ])
);
for (const [signal, handler] of Object.entries(shutdownHandlers)) {
  process.on(signal, handler);
}

function removeShutdownHandlers() {
  for (const [signal, handler] of Object.entries(shutdownHandlers)) {
    process.removeListener(signal, handler);
  }
}

function throwIfShuttingDown() {
  if (shutdownSignal !== null) throw shutdownError();
}

function interruptible(operation) {
  throwIfShuttingDown();
  return Promise.race([
    operation,
    shutdownRequested.then((signal) => {
      throw shutdownError(signal);
    }),
  ]);
}

function interruptibleMutation(operation) {
  return interruptible(inFlightMutations.track(operation));
}

function recoveryManifest(attempt) {
  return {
    schemaVersion: 1,
    recovery: true,
    createdAtMs: attempt.createdAtMs ?? null,
    gameIds: [...attempt.gameIds],
    fleetStarted: attempt.fleetStarted === true,
  };
}

function persistRecoveryManifest(attempt) {
  A.writeState(recoveryManifest(attempt));
}

async function teardownSavedState(state) {
  const errors = [];
  const attempt = async (label, action) => {
    try {
      await action();
    } catch (error) {
      errors.push(`${label}: ${error.message}`);
    }
  };

  await attempt('VPN team clients', () => teardownVpnTeamClients(state));
  await attempt('BYOC fleet', () => A.teardownFleet(state));
  await attempt('bootstrap hill', () => A.teardownHill());
  if (state) {
    await attempt('event namespace', () => {
      const gameIds = currentAttemptGameIds(state);
      return gameIds.length ? A.teardownNamespace(gameIds) : undefined;
    });
  }
  if (errors.length) {
    throw new Error(`teardown incomplete; state retained for retry: ${errors.join('; ')}`);
  }
  rmSync(A.stateFile, { force: true });
}

function exactReadinessMatches(epoch, evidence, crown, expectedServices) {
  return (
    epoch.startRound > 0 &&
    epoch.liveRound >= epoch.startRound &&
    epoch.flagsPublished === true &&
    epoch.plantedFlags === expectedServices &&
    evidence.liveRound === epoch.liveRound &&
    evidence.requestedServices === expectedServices &&
    evidence.plantedFlags === expectedServices &&
    evidence.deliveredFlags === expectedServices &&
    evidence.verifiedFlags === expectedServices &&
    crown.phase === 'Active' &&
    crown.rosterCount === expectedServices &&
    crown.tokenCount === expectedServices &&
    crown.containerId &&
    crown.containerId === crown.replacementContainerId
  );
}

async function pauseAtVerifiedReadiness(gameId, adChallengeId, kothChallengeId, participationIds, initialEpoch) {
  const expectedServices = participationIds.length;
  let afterRound = Math.max(0, Number(initialEpoch.startRound) - 1);
  let observed = null;
  for (let attempt = 1; attempt <= 4; attempt++) {
    throwIfShuttingDown();
    await interruptible(A.waitForFleetExactEvidence(gameId, adChallengeId, participationIds, { afterRound }));
    await interruptible(A.waitForCrownReady(gameId, kothChallengeId, expectedServices));
    const boardWarmMs = await interruptible(A.warmEpochBoard(gameId, expectedServices));
    await interruptibleMutation(A.setAdScoringPaused(gameId, true));

    const epoch = A.epochReadiness(gameId);
    const evidence = A.fleetExactReadiness(gameId, adChallengeId, participationIds);
    const crown = A.crownReadiness(gameId, kothChallengeId);
    observed = { epoch, evidence, crown };
    if (
      epoch.startRound === initialEpoch.startRound &&
      exactReadinessMatches(epoch, evidence, crown, expectedServices)
    ) {
      return { epoch, evidence, crown, boardWarmMs };
    }

    // A scheduler boundary can land between the final readiness read and the
    // pause API. Resume only long enough to obtain another complete setup round,
    // then retry the same atomic handoff.
    await interruptibleMutation(A.setAdScoringPaused(gameId, false));
    afterRound = Math.max(afterRound, Number(epoch.liveRound) || 0);
    console.log(`  readiness moved during pause handoff; retrying after round ${afterRound} (${attempt}/4)`);
  }
  throw new Error(`could not pause a fully verified competition round: ${JSON.stringify(observed)}`);
}

async function main() {
  orchestrationLock = await acquireExclusiveProcessLock(loadOrchestrationLockPath, {
    label: 'RSCTF lifecycle provisioning',
    inheritedToken: process.env.RSCTF_LOAD_ORCHESTRATION_LOCK_TOKEN,
    metadata: { stateTag: process.env.LIFECYCLE_STATE_TAG || null },
  });
  throwIfShuttingDown();

  if (process.argv.includes('--down')) {
    const st = A.readState();
    await teardownSavedState(st);
    console.log('torn down');
    return;
  }

  await interruptible(A.preflight());
  console.log('preflight ok (admin JWT valid)');

  const previousState = A.readState();
  if (previousState) {
    const forceReprovision = process.env.REPROVISION === '1' || process.env.PROVISION === '1';
    if (!forceReprovision) {
      throw new Error('a lifecycle namespace already exists; run npm run teardown first, or set REPROVISION=1');
    }
    console.log('removing the previous lifecycle namespace before reprovisioning…');
    await teardownSavedState(previousState);
  }

  activeProvision = {
    gameIds: [],
    fleetStarted: false,
  };

  const now = A.nowMs();
  const competitionRunId = REALISTIC_COMPETITION ? randomUUID() : null;
  activeProvision.createdAtMs = now;
  persistRecoveryManifest(activeProvision);
  const start = now - 3600_000;
  const eventDurationMs = EVENT_DURATION_SECONDS * 1000;
  if (!Number.isSafeInteger(now + eventDurationMs)) {
    throw new Error('EVENT_DURATION_SECONDS produces an invalid event end time');
  }
  const end = now + eventDurationMs;

  // ── G_JEO: a Jeopardy event ────────────────────────────────────────────────
  const jeoGame = await interruptibleMutation(A.createGame({
    title: `LOADTEST-JEO-${now}`,
    hidden: false,
    practiceMode: false,
    acceptWithoutReview: true,
    start,
    end,
    teamMemberCountLimit: 0,
    containerCountLimit: 3,
    allowUserSubmissions: false,
  }));
  activeProvision.gameIds.push(jeoGame);
  persistRecoveryManifest(activeProvision);
  console.log(`G_JEO = ${jeoGame}`);
  const staticFlags = {};
  const staticCatalog = [];
  const jeopardyCategories = ['Web', 'Pwn', 'Crypto', 'Misc', 'Reverse'];
  for (let i = 1; i <= CH_STATIC; i++) {
    throwIfShuttingDown();
    const category = jeopardyCategories[(i - 1) % jeopardyCategories.length];
    const difficulty = 2 + ((i - 1) % 5);
    const cid = await interruptibleMutation(A.createChallenge(jeoGame, {
      title: `jeo-${i}`,
      category,
      type: 'StaticAttachment',
    }));
    await interruptibleMutation(A.setChallenge(jeoGame, cid, {
      content: 'loadtest',
      originalScore: 1000,
      minScoreRate: 0.25,
      difficulty,
    }));
    const flag = `flag{loadtest_${cid}}`;
    await interruptibleMutation(A.addFlags(jeoGame, cid, [flag]));
    await interruptibleMutation(A.setChallenge(jeoGame, cid, { isEnabled: true }));
    staticFlags[cid] = flag;
    staticCatalog.push({
      challengeId: cid,
      kind: i === 1 ? 'attachment' : 'static',
      category,
      difficulty,
      flag,
      attachmentPath: null,
    });
  }
  // Give the first static challenge a real downloadable attachment (stresses /assets serving).
  const attachName = 'challenge.zip';
  const attachHash = await interruptibleMutation(
    A.uploadAsset(attachName, 'loadtest-attachment-payload-'.repeat(500))
  );
  await interruptibleMutation(A.setAttachment(jeoGame, Number(Object.keys(staticFlags)[0]), attachHash));
  staticCatalog[0].attachmentPath = `/assets/${attachHash}/${attachName}`;
  console.log(`  attachment uploaded (${attachHash.slice(0, 12)}…)`);
  // one container challenge for the container cohort
  const containerChal = await interruptibleMutation(A.createChallenge(jeoGame, {
    title: 'jeo-box',
    category: 'Web',
    type: 'StaticContainer',
  }));
  await interruptibleMutation(A.setChallenge(jeoGame, containerChal, {
    containerImage: CONTAINER_IMAGE,
    memoryLimit: 64,
    cpuCount: 1,
    exposePort: 80,
    enableTrafficCapture: false,
  }));
  await interruptibleMutation(
    A.rebuildChallengeImage(jeoGame, containerChal, CONTAINER_IMAGE, 'Jeopardy container challenge')
  );
  const containerFlag = `flag{loadtest_${containerChal}}`;
  await interruptibleMutation(A.addFlags(jeoGame, containerChal, [containerFlag]));
  await interruptibleMutation(A.setChallenge(jeoGame, containerChal, { isEnabled: true }));
  const jeopardyCatalog = [
    ...staticCatalog,
    {
      challengeId: containerChal,
      kind: 'container',
      category: 'Web',
      difficulty: 5,
      flag: containerFlag,
      attachmentPath: null,
    },
  ];
  console.log(`  ${CH_STATIC} static + 1 container challenge`);

  // ── G_MIX: A&D + KotH ──────────────────────────────────────────────────────
  const mixGame = await interruptibleMutation(A.createGame({
    title: `LOADTEST-MIX-${now}`,
    hidden: false,
    practiceMode: false,
    acceptWithoutReview: true,
    start,
    end,
    adTickSeconds: 30,
    adFlagLifetimeTicks: 5,
    adGetflagWindowFraction: 0.9,
    adMinGracePeriodSeconds: 1,
    adResetCooldownMinutes: 5,
    ...KOTH_CONFIG,
  }));
  activeProvision.gameIds.push(mixGame);
  persistRecoveryManifest(activeProvision);
  console.log(`G_MIX = ${mixGame}`);
  if (REALISTIC_COMPETITION) await interruptibleMutation(A.setAdScoringPaused(mixGame, true));
  // create_game (POST) does not persist these engine tuning fields, so the
  // lifecycle fixture sets the crown-cycle configuration directly.
  sql(
    `UPDATE "Games" SET ad_warmup_seconds=1, ad_tick_seconds=30, ad_flag_lifetime_ticks=5,` +
      ` ad_getflag_window_fraction=0.9, ad_min_grace_period_seconds=1,` +
      ` koth_epoch_ticks=${KOTH_CONFIG.kothEpochTicks}, koth_cycle_ticks=${KOTH_CONFIG.kothCycleTicks},` +
      ` koth_champion_cooldown_ticks=${KOTH_CONFIG.kothChampionCooldownTicks},` +
      ` koth_claim_confirmation_ticks=${KOTH_CONFIG.kothClaimConfirmationTicks}` +
      ` WHERE id=${mixGame}`
  );
  const adChal = await interruptibleMutation(A.createChallenge(mixGame, {
    title: 'ad-svc',
    category: 'Pwn',
    type: 'AttackDefense',
  }));
  const checkerDir = A.prepareExactChecker(mixGame, adChal);
  await interruptibleMutation(A.setChallenge(mixGame, adChal, {
    adSelfHosted: true,
    adAllowSelfReset: true,
    adCheckerImage: checkerDir,
  })); // BYOC: teams bring a tunnel; official scoring requires this prepared exact checker
  await interruptibleMutation(A.addFlags(mixGame, adChal, ['flag{ad_placeholder}'])); // enable-gate needs a flag; A&D plants dynamic flags per round
  await interruptibleMutation(A.setChallenge(mixGame, adChal, { isEnabled: true }));
  const kothChal = await interruptibleMutation(A.createChallenge(mixGame, {
    title: 'the-hill',
    category: 'Pwn',
    type: 'KingOfTheHill',
  }));
  const kothImage =
    KOTH_CONTAINER_OVERRIDE?.image ??
    (REALISTIC_COMPETITION ? A.buildCompetitiveKothImage() : CONTAINER_IMAGE);
  const kothPort =
    KOTH_CONTAINER_OVERRIDE?.port ?? (REALISTIC_COMPETITION ? 8080 : 80);
  const kothCheckerDir = A.prepareKothChecker(mixGame, kothChal);
  await interruptibleMutation(A.setChallenge(mixGame, kothChal, {
    containerImage: kothImage,
    memoryLimit: 64,
    cpuCount: 1,
    exposePort: kothPort,
    adAllowEgress: false,
    adCheckerImage: kothCheckerDir,
  }));
  await interruptibleMutation(
    A.rebuildChallengeImage(mixGame, kothChal, kothImage, 'KotH challenge')
  );
  await interruptibleMutation(A.addFlags(mixGame, kothChal, ['flag{koth_placeholder}']));
  await interruptibleMutation(A.setChallenge(mixGame, kothChal, { isEnabled: true }));
  console.log(`  A&D chal ${adChal} + KotH chal ${kothChal}`);

  // ── Seed cohorts ───────────────────────────────────────────────────────────
  console.log(`seeding ${TEAMS_JEO} jeopardy + ${TEAMS_AD} A&D/KotH teams…`);
  throwIfShuttingDown();
  const jeo = A.seedCohort(jeoGame, TEAMS_JEO);
  const mix = A.seedCohort(mixGame, TEAMS_AD);
  const fleetService = A.startFleetService(mixGame, adChal);
  const serviceAddress = fleetService.checker;
  const [serviceHost, servicePort] = serviceAddress.split(':');
  A.seedAdServices(mixGame, adChal, serviceHost, Number(servicePort));
  if (REALISTIC_COMPETITION) {
    if (process.env.LIFECYCLE_ISOLATED_SERVICES !== '1') {
      throw new Error('realistic competition provisioning requires LIFECYCLE_ISOLATED_SERVICES=1');
    }
    activeProvision.fleetStarted = true;
    persistRecoveryManifest(activeProvision);
    A.startFleetForPids(mixGame, adChal, mix.partIds, fleetService.tunnel);
    const readyTunnels = await interruptible(A.waitForFleetReady(mixGame, adChal, mix.partIds));
    console.log(`  initial BYOC fleet: ${readyTunnels}/${mix.partIds.length} tunnels up`);
  }
  A.seedKothTarget(mixGame, kothChal);
  const kothContainer = A.startHill(mixGame, kothChal, kothImage, kothPort); // bootstrap with the snapshotted image
  console.log(`  KotH hill container ${kothContainer.slice(0, 12)}`);
  if (REALISTIC_COMPETITION) {
    await interruptibleMutation(A.setAdScoringPaused(mixGame, false));
    console.log('  official scoring resumed after every team service became reachable');
  }

  // ── Wait for the scheduler-owned official scoring boundary ─────────────────
  console.log('  waiting for automatic epoch scoring readiness…');
  let epoch = await interruptible(A.waitForEpochBoundary(mixGame, mix.partIds.length));
  let crown;
  let readinessEvidence = null;
  let boardWarmMs;
  if (REALISTIC_COMPETITION) {
    const readiness = await pauseAtVerifiedReadiness(mixGame, adChal, kothChal, mix.partIds, epoch);
    epoch = readiness.epoch;
    crown = readiness.crown;
    readinessEvidence = readiness.evidence;
    boardWarmMs = readiness.boardWarmMs;
  } else {
    crown = await interruptible(A.waitForCrownReady(mixGame, kothChal, mix.partIds.length));
    boardWarmMs = await interruptible(A.warmEpochBoard(mixGame, mix.partIds.length));
  }
  const planted = A.plantedFlags(mixGame);
  if (planted.length !== epoch.rosterServices) {
    throw new Error(`planted flag snapshot changed during readiness check (${planted.length}/${epoch.rosterServices})`);
  }
  const kothTokens = A.kothCapturable(mixGame, kothChal);
  if (kothTokens.length !== mix.partIds.length) {
    throw new Error(`automatic round did not mint every KotH token (${kothTokens.length}/${mix.partIds.length})`);
  }
  console.log(
    `  epoch ready: start round ${epoch.startRound}, frozen ${epoch.rosterTeams} teams / ` +
      `${epoch.rosterServices} services, publication settled, ${planted.length} current flags`
  );
  console.log(
    `  crown ready: cycle ${crown.cycleNumber} ${crown.phase}, ` +
      `${crown.tokenCount}/${crown.rosterCount} scoped capabilities, container ${crown.containerId.slice(0, 12)}`
  );
  console.log(`  official board rollups warmed in ${boardWarmMs} ms`);
  if (REALISTIC_COMPETITION) {
    console.log(`  official scoring paused on verified readiness round ${readinessEvidence.liveRound}`);
  }

  const adminUuid = A.adminUuid();
  const userStamps = Object.fromEntries(
    [...new Set([...jeo.userIds, ...mix.userIds, adminUuid])].map((id) => [
      id,
      sql(`SELECT security_stamp FROM "AspNetUsers" WHERE id='${id}'::uuid`),
    ])
  );
  A.writeState({
    schemaVersion: 1,
    recovery: false,
    createdAtMs: now,
    ...(REALISTIC_COMPETITION
      ? {
          competitionRunId,
          competitionModelVersion: 2,
          competitionSeed: COMPETITION_SEED,
        }
      : {}),
    gameIds: [jeoGame, mixGame],
    jeoGame,
    mixGame,
    adChal,
    kothChal,
    containerChal,
    staticFlags,
    jeopardyCatalog,
    jeoUsers: jeo.userIds,
    jeoTeamIds: jeo.teamIds,
    jeoPartIds: jeo.partIds,
    adUsers: mix.userIds,
    adTeamIds: mix.teamIds,
    adPartIds: mix.partIds, // for the BYOC agent fleet (real per-participation tokens)
    plantedFlags: planted,
    dedupFlag: planted[0]?.flag || null,
    attackerUuid: mix.userIds[0],
    attachHash,
    attachName,
    attachChal: Number(Object.keys(staticFlags)[0]),
    kothContainer: crown.containerId,
    kothContainerImage: kothImage,
    kothContainerPort: kothPort,
    initialKothContainer: kothContainer,
    crownCycleId: crown.cycleId,
    epochStartRound: epoch.startRound,
    ...(REALISTIC_COMPETITION
      ? {
          scoringPausedAfterReadiness: true,
          readinessRound: readinessEvidence.liveRound,
        }
      : {}),
    adminUuid,
    userStamps,
  });
  activeProvision = null;
  console.log(
    `state written: G_JEO=${jeoGame} (${TEAMS_JEO} teams), G_MIX=${mixGame} (${TEAMS_AD} teams), ${planted.length} planted flags`
  );
}

main()
  .catch(async (error) => {
    const failure = error instanceof Error ? error : new Error(String(error));
    // A shutdown can win Promise.race while an HTTP create/update still owns a
    // socket. Let it settle before resource discovery so cleanup sees its final
    // committed state and cannot remove the recovery manifest too early.
    await inFlightMutations.drain();
    if (activeProvision) {
      const cleanupErrors = await cleanupFailedProvision(activeProvision);
      if (cleanupErrors.length === 0) {
        rmSync(A.stateFile, { force: true });
      } else {
        try {
          persistRecoveryManifest(activeProvision);
        } catch (manifestError) {
          cleanupErrors.push(`persist recovery manifest: ${manifestError.message}`);
        }
      }
      if (cleanupErrors.length) {
        failure.message += `; cleanup errors: ${cleanupErrors.join('; ')}`;
      }
    }
    console.error('provision failed:', failure.message);
    process.exitCode = shutdownSignal === 'SIGINT' ? 130 : shutdownSignal === 'SIGTERM' ? 143 : 1;
  })
  .finally(async () => {
    removeShutdownHandlers();
    await orchestrationLock?.release();
  });
