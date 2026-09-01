// App-level helpers for the whole-platform lifecycle stress test — built on lib.mjs.
// Covers the organizer setup surface (/api/edit), bulk cohort seeding (SQL), the A&D
// round engine, the live auth/team/join flow, and namespaced teardown. Everything the
// harness creates is namespaced (@load.test emails, LT<gid>_ team names, fresh game ids)
// so the shared host + games 9/10 are never touched.
import { createHash, randomUUID } from 'node:crypto';
import { chmodSync, existsSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import {
  TARGET,
  sql,
  docker,
  mintJwt,
  byocCapabilitiesForPids,
  byocRsctfIp,
  rsctfIp,
  sleep,
  byocAgentImage,
  NET,
  RSCTF,
  retryTransientUntil,
} from './lib.mjs';
import { cohortSeedQuery, parseCohortSeedResult } from './cohort-seed.js';
import { materializeFixtures } from './fixtures.mjs';
import {
  assertImmutableBuildRecord,
  assertSuccessfulBuildJob,
  isImmutableImageReference,
} from './fixture-image-config.js';
import { retainedManifestMatchesGame } from './retention-identity.mjs';
import { dockerOwnershipLabelArgs, dockerScopeFromContainerEnv } from './docker-scope.js';
import { BOUNDED_DOCKER_LOG_ARGS } from './docker-runtime.js';
import {
  dockerLabelArgs,
  dockerOwnershipFilterArgs,
  fleetLabelKeys,
  fleetLabels,
  fleetParticipantBindings,
  normalizeFleetScope,
  ownsFleetResource,
} from './fleet-ownership.js';
import { kothObservationHeaders } from './koth-api-observer.js';
import { managedKothOperationCycleId } from './managed-koth-model.js';

const STATE_DIRECTORY = new URL('.', import.meta.url).pathname;
const STATE_TAG = String(process.env.LIFECYCLE_STATE_TAG || '').trim();
if (STATE_TAG && !/^[a-z0-9][a-z0-9-]{0,31}$/.test(STATE_TAG)) {
  throw new Error('LIFECYCLE_STATE_TAG must contain 1-32 lowercase letters, digits, or hyphens');
}
const STATE_BASENAME = STATE_TAG ? `.lifecycle-state-${STATE_TAG}.json` : '.lifecycle-state.json';
const STATE = `${STATE_DIRECTORY}${STATE_BASENAME}`;
const STATE_MANIFEST_PATTERN = /^\.lifecycle-state(?:-[a-z0-9][a-z0-9-]{0,31})?\.json$/;
const AD_NET = process.env.AD_NET || 'rsctf-ad';
export const nowMs = () => Number(sql('SELECT (extract(epoch from now())*1000)::bigint')); // clock via DB (no Date in workflows, fine here)

// ── HTTP ──────────────────────────────────────────────────────────────────────
/**
 * One API call. Returns { status, json, text }. jwt → Bearer; ip → the legacy
 * X-Real-IP load/rate-limit header. Identity-admission scenarios must supply
 * an explicit X-Forwarded-For chain through their trusted test proxy.
 */
export async function api(
  method,
  path,
  {
    jwt,
    ip,
    body,
    rawBody,
    headers: suppliedHeaders,
    timeoutMs = 30_000,
    baseUrl = TARGET,
  } = {},
) {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new Error(`API timeout must be a positive number (got ${timeoutMs})`);
  }
  if (body !== undefined && rawBody !== undefined) {
    throw new Error('API request cannot provide both body and rawBody');
  }
  const headers = { 'content-type': 'application/json', ...(suppliedHeaders || {}) };
  if (jwt) headers.authorization = `Bearer ${jwt}`;
  if (ip) headers['x-real-ip'] = ip;
  const r = await fetch(`${baseUrl}${path}`, {
    method,
    headers,
    body: rawBody !== undefined ? rawBody : body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const text = await r.text();
  let json;
  try {
    json = text ? JSON.parse(text) : undefined;
  } catch {
    json = undefined;
  }
  return { status: r.status, json, text, headers: r.headers };
}

// ── Admin identity ──────────────────────────────────────────────────────────────
export function adminUuid() {
  return sql(`SELECT id FROM "AspNetUsers" WHERE role=3 ORDER BY register_time_utc LIMIT 1`);
}
export function adminJwt() {
  return mintJwt(adminUuid());
}

/** Fail fast if the JWT secret is wrong — every auth call would otherwise silently 401. */
export async function preflight() {
  const r = await api('GET', '/api/account/profile', { jwt: adminJwt() });
  if (r.status !== 200 || r.json?.role !== 'Admin') {
    throw new Error(
      `preflight failed (status ${r.status}, role ${r.json?.role}). Wrong RSCTF_JWT_SECRET? ` +
        `Set it to the rsctf container's configured JWT secret.`
    );
  }
  return true;
}

// ── Organizer setup (/api/edit, admin JWT) ────────────────────────────────────
const jwtOpt = () => ({ jwt: adminJwt(), ip: '10.9.9.9' });
async function must(r, what) {
  if (r.status >= 300) throw new Error(`${what} → ${r.status} ${r.text?.slice(0, 200)}`);
  return r;
}
const unwrap = (r) => (r.json && 'data' in r.json ? r.json.data : r.json);

export async function createGame(body) {
  const r = await must(
    await api('POST', '/api/edit/games', {
      ...jwtOpt(),
      body,
      headers: { 'idempotency-key': randomUUID() },
    }),
    'createGame',
  );
  return unwrap(r).id;
}
export async function setGameSchedule(gid, start, end) {
  if (![gid, start, end].every(Number.isSafeInteger) || gid <= 0 || start >= end) {
    throw new Error('setGameSchedule requires a valid game and increasing millisecond timestamps');
  }
  const current = unwrap(
    await must(await api('GET', `/api/edit/games/${gid}`, jwtOpt()), 'getGame')
  );
  return must(
    await api('PUT', `/api/edit/games/${gid}`, {
      ...jwtOpt(),
      body: { ...current, start, end, operationId: randomUUID() },
    }),
    'setGameSchedule'
  );
}
export async function createChallenge(gid, body) {
  const r = await must(
    await api('POST', `/api/edit/games/${gid}/challenges`, {
      ...jwtOpt(),
      body: { ...body, operationId: body.operationId || randomUUID() },
    }),
    'createChallenge'
  );
  return unwrap(r).id;
}
export async function setChallenge(gid, cid, body) {
  const current = unwrap(
    await must(await api('GET', `/api/edit/games/${gid}/challenges/${cid}`, jwtOpt()), 'getChallenge')
  );
  return must(
    await api('PUT', `/api/edit/games/${gid}/challenges/${cid}`, {
      ...jwtOpt(),
      body: {
        ...body,
        operationId: body.operationId || randomUUID(),
        expectedRevision: body.expectedRevision ?? current.revision,
      },
    }),
    'setChallenge'
  );
}

const CONTROL_JOB_STATUSES = new Set(['Queued', 'Running', 'Succeeded', 'Failed', 'Cancelled']);
const TERMINAL_CONTROL_JOB_STATUSES = new Set(['Succeeded', 'Failed', 'Cancelled']);

/** Wait for one durable operator job, tolerating bounded transient read failures. */
export async function waitForControlJob(initial, label = 'control job', timeoutMs = 15 * 60_000) {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new TypeError('control-job timeout must be positive');
  }
  const requireJob = (value) => {
    if (typeof value?.id !== 'string' || !CONTROL_JOB_STATUSES.has(value?.status)) {
      throw new Error(`${label} did not return a valid control job`);
    }
    return value;
  };
  let job = requireJob(initial);
  const deadline = performance.now() + timeoutMs;
  let delayMs = 250;
  while (!TERMINAL_CONTROL_JOB_STATUSES.has(job.status)) {
    const remainingMs = deadline - performance.now();
    if (remainingMs <= 0) {
      throw new Error(`${label} did not finish within ${Math.ceil(timeoutMs / 60_000)} minutes`);
    }
    await sleep(Math.min(delayMs, remainingMs));
    const polled = await retryTransientUntil(
      () => api('GET', `/api/edit/jobs/${job.id}`, jwtOpt()),
      (candidate) => candidate instanceof Error || candidate.status === 429 || candidate.status >= 500,
      { budgetMs: Math.min(10_000, remainingMs), delayMs: 500 },
    );
    job = requireJob(unwrap(await must(polled, `poll ${label}`)));
    delayMs = Math.min(2_000, Math.ceil(delayMs * 1.5));
  }
  if (job.status !== 'Succeeded') {
    const diagnostic = typeof job.error === 'string' ? `: ${job.error}` : '';
    throw new Error(`${label} ended as ${job.status}${diagnostic}`);
  }
  return job;
}

/** Rebuild one exact challenge and verify the immutable result committed. */
export async function rebuildChallengeImage(gid, cid, requestedImage, label = 'challenge') {
  const gameId = Number(gid);
  const challengeId = Number(cid);
  if (
    !Number.isSafeInteger(gameId) ||
    gameId <= 0 ||
    !Number.isSafeInteger(challengeId) ||
    challengeId <= 0 ||
    typeof requestedImage !== 'string' ||
    requestedImage.trim().length === 0
  ) {
    throw new Error('immutable rebuild requires valid game/challenge ids and an image');
  }

  const response = await must(
    await api('POST', `/api/edit/games/${gameId}/challenges/${challengeId}/rebuild`, {
      ...jwtOpt(),
      headers: { 'idempotency-key': randomUUID() },
    }),
    `rebuild ${label}`
  );
  const job = await waitForControlJob(unwrap(response), `${label} immutable rebuild`);
  assertSuccessfulBuildJob(job, label);

  // The rebuild wire model intentionally exposes status/log only. Read back the
  // durable identity so provisioning never races ahead on a successful-looking
  // response whose compare-and-swap did not publish the requested definition.
  const raw = sql(
    `SELECT json_build_object(` +
      `'containerImage',container_image,` +
      `'buildStatus',build_status,` +
      `'buildImageDigest',build_image_digest` +
      `)::text FROM "GameChallenges" WHERE game_id=${gameId} AND id=${challengeId}`
  );
  if (!raw) throw new Error(`${label} disappeared after its immutable rebuild`);
  return assertImmutableBuildRecord(JSON.parse(raw), requestedImage, label);
}
export async function addFlags(gid, cid, flags) {
  const body = {
    operationId: randomUUID(),
    flags: flags.map((f) => ({ flag: f })),
  };
  return must(
    await api('POST', `/api/edit/games/${gid}/challenges/${cid}/flags`, {
      ...jwtOpt(),
      body,
    }),
    'addFlags'
  );
}
export async function deleteGame(gid) {
  return api('DELETE', `/api/edit/games/${gid}`, {
    ...jwtOpt(),
    timeoutMs: 120_000,
  });
}

const LOAD_GAME_TITLE_RE = /^(?:LOADTEST-[A-Za-z0-9-]+|MULTI-DOMAIN-[a-z0-9][a-z0-9-]{0,31})$/;

function isDeletionProtectedByEvidence(response) {
  const detail = (typeof response?.json?.detail === 'string' ? response.json.detail : response?.text) || '';
  const body = String(detail).toLowerCase();
  return body.includes('cannot be permanently deleted') || body.includes('competition evidence');
}

function retryAfterSeconds(response) {
  const retryAfter = response?.headers?.get?.("retry-after");
  if (!retryAfter) return 0;
  const numeric = Number(retryAfter);
  return Number.isFinite(numeric) && numeric > 0 ? numeric : 0;
}

async function deleteGameWithRetry(gid, { attempts = 6, jitterMs = 250 } = {}) {
  let last;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    last = await deleteGame(gid);
    if (last.status < 300) return last;
    if (last.status !== 429) return last;
    const backoff = 250 + retryAfterSeconds(last) * 1000 + attempt * jitterMs * 5;
    await sleep(backoff);
  }
  return last;
}

async function exactLoadGameCleanup(gameId, title, runtimeIds = []) {
  const { deleteDisposableLoadGame } = await import('./admin-fixtures.mjs');
  return deleteDisposableLoadGame(gameId, title, { runtimeIds });
}

async function clearDisposableLoadBlobOwners(gameId, title) {
  if (!LOAD_GAME_TITLE_RE.test(title)) {
    throw new Error(`refusing blob cleanup for non-load game ${gameId}`);
  }
  const currentTitle = sql(`SELECT title FROM "Games" WHERE id=${gameId}`);
  if (currentTitle !== title) {
    throw new Error(`load game ${gameId} identity changed before blob cleanup`);
  }

  await must(
    await api('DELETE', `/api/edit/games/${gameId}/writeups`, {
      ...jwtOpt(),
      timeoutMs: 120_000,
    }),
    `clear load game ${gameId} writeups`,
  );
  const attachmentIds = sql(
    `SELECT id FROM "GameChallenges" ` +
      `WHERE game_id=${gameId} AND attachment_id IS NOT NULL ORDER BY id`,
  )
    .split(/\s+/)
    .filter(Boolean)
    .map(Number);
  for (const challengeId of attachmentIds) {
    if (!Number.isSafeInteger(challengeId) || challengeId <= 0) {
      throw new Error(`invalid attachment owner for load game ${gameId}`);
    }
    await must(
      await api('POST', `/api/edit/games/${gameId}/challenges/${challengeId}/attachment`, {
        ...jwtOpt(),
        body: { attachmentType: 'None' },
      }),
      `clear load challenge ${challengeId} attachment`,
    );
  }
}

async function exactProtectedLoadGameCleanup(gameId, title, runtimeIds) {
  // Stop the round engine before releasing fixture-owned blobs. The exact SQL
  // fallback still independently locks the game and refuses any remaining
  // poster/archive/flag attachment owner, so this cannot become a generic
  // competition-history deletion path.
  await setAdScoringPaused(gameId, true);
  await clearDisposableLoadBlobOwners(gameId, title);
  return exactLoadGameCleanup(gameId, title, { runtimeIds });
}

/** Idempotently pause/resume the official A&D/KotH scoring clock through its admin API. */
export async function setAdScoringPaused(gid, desired) {
  const gameId = Number(gid);
  if (!Number.isSafeInteger(gameId) || gameId <= 0 || typeof desired !== 'boolean') {
    throw new Error(`invalid scoring-pause request ${gid}/${desired}`);
  }
  const rawCurrent = sql(
    `SELECT json_build_object('paused',ad_scoring_paused,'revision',ad_control_revision)::text ` +
      `FROM "Games" WHERE id=${gameId}`,
  );
  if (!rawCurrent) throw new Error(`cannot pause missing game ${gameId}`);
  const current = JSON.parse(rawCurrent);
  if (current.paused === desired) return desired;
  const response = await must(
    await api('POST', `/api/edit/games/${gameId}/ad/ScoringPause`, {
      ...jwtOpt(),
      body: { paused: desired, revision: Number(current.revision) },
    }),
    `${desired ? 'pause' : 'resume'} scoring`
  );
  const model = unwrap(response);
  if (model?.scoringPaused !== desired) {
    throw new Error(`scoring pause API returned an inconsistent state for game ${gameId}`);
  }
  const stored = sql(`SELECT ad_scoring_paused::text FROM "Games" WHERE id=${gameId}`);
  if ((stored === 'true' || stored === 't') !== desired) {
    throw new Error(`scoring pause state did not persist for game ${gameId}`);
  }
  return desired;
}

export function adScoringPaused(gid) {
  const gameId = Number(gid);
  if (!Number.isSafeInteger(gameId) || gameId <= 0) {
    throw new Error(`invalid scoring-pause query ${gid}`);
  }
  const stored = sql(`SELECT ad_scoring_paused::text FROM "Games" WHERE id=${gameId}`);
  if (!stored) throw new Error(`cannot inspect missing game ${gameId}`);
  return stored === 'true' || stored === 't';
}

// ── A&D round engine ──────────────────────────────────────────────────────────
/** Currently-live planted flags for a game: [{flag, pid}] (each = a valid capture of that team). */
export function plantedFlags(gid) {
  const rows = sql(
    `SELECT af.flag||'|'||ats.participation_id FROM "AdFlags" af
       JOIN "AdTeamServices" ats ON ats.id=af.team_service_id
       JOIN "AdRounds" r ON r.id=af.round_id
      WHERE r.game_id=${gid} AND r.finalized=false`
  );
  return (rows || '')
    .split('\n')
    .filter(keepId)
    .map((l) => {
      const [flag, pid] = l.split('|');
      return { flag, pid: Number(pid) };
    });
}

/** Observable contract for the official epoch board's immutable start boundary. */
export function epochReadiness(gid) {
  const gameId = Number(gid);
  if (!Number.isSafeInteger(gameId) || gameId <= 0) throw new Error(`invalid A&D game id ${gid}`);
  const row = sql(
    `SELECT json_build_object(` +
      `'startRound',game.ad_scoring_start_round,` +
      `'checkerCount',(SELECT count(*) FROM "GameChallenges" challenge ` +
      `WHERE challenge.game_id=game.id AND challenge."Type"=4 AND challenge.is_enabled AND challenge.review_status=0),` +
      `'checkerPaths',(SELECT COALESCE(json_agg(challenge.ad_checker_image ORDER BY challenge.id), '[]'::json) ` +
      `FROM "GameChallenges" challenge ` +
      `WHERE challenge.game_id=game.id AND challenge."Type"=4 AND challenge.is_enabled AND challenge.review_status=0 ` +
      `AND challenge.ad_checker_image IS NOT NULL AND btrim(challenge.ad_checker_image)<>''),` +
      `'rosterTeams',(SELECT count(DISTINCT service.participation_id) FROM "AdRounds" round ` +
      `JOIN "AdFlags" flag ON flag.round_id=round.id ` +
      `JOIN "AdTeamServices" service ON service.id=flag.team_service_id ` +
      `WHERE round.game_id=game.id AND round.number=game.ad_scoring_start_round),` +
      `'rosterServices',(SELECT count(*) FROM "AdRounds" round ` +
      `JOIN "AdFlags" flag ON flag.round_id=round.id ` +
      `JOIN "AdTeamServices" service ON service.id=flag.team_service_id ` +
      `WHERE round.game_id=game.id AND round.number=game.ad_scoring_start_round),` +
      `'liveRound',(SELECT max(round.number) FROM "AdRounds" round ` +
      `WHERE round.game_id=game.id AND round.finalized=false),` +
      `'flagsPublished',COALESCE((SELECT round.flags_published_at IS NOT NULL FROM "AdRounds" round ` +
      `WHERE round.game_id=game.id AND round.finalized=false ORDER BY round.number DESC LIMIT 1),false),` +
      `'plantedFlags',(SELECT count(*) FROM "AdRounds" round ` +
      `JOIN "AdFlags" flag ON flag.round_id=round.id ` +
      `WHERE round.game_id=game.id AND round.finalized=false),` +
      `'verifiedFlags',(SELECT count(*) FROM "AdRounds" round ` +
      `JOIN "AdCheckResults" result ON result.round_id=round.id ` +
      `WHERE round.game_id=game.id AND round.finalized=false AND result.status=0 ` +
      `AND result.flag_verified=true AND result.sla_credit IS NOT NULL)` +
      `)::text FROM "Games" game WHERE game.id=${gameId}`
  );
  if (!row) throw new Error(`A&D game ${gid} does not exist`);
  const snapshot = JSON.parse(row);
  snapshot.preparedCheckerCount = snapshot.checkerPaths.filter((path) => {
    const python = docker(['exec', RSCTF, 'test', '-x', `${path}/venv/bin/python3`]);
    const entrypoint = docker(['exec', RSCTF, 'test', '-f', `${path}/src/run.py`]);
    return python.status === 0 && entrypoint.status === 0;
  }).length;
  return snapshot;
}

/** SQL-only live-round snapshot for the timed loop (no checker-container execs). */
export function liveEpochSnapshot(gid) {
  const gameId = Number(gid);
  if (!Number.isSafeInteger(gameId) || gameId <= 0) throw new Error(`invalid A&D game id ${gid}`);
  const row = sql(
    `SELECT json_build_object(` +
      `'liveRound',(SELECT max(round.number) FROM "AdRounds" round ` +
      `WHERE round.game_id=game.id AND round.finalized=false),` +
      `'plantedFlags',(SELECT count(*) FROM "AdRounds" round ` +
      `JOIN "AdFlags" flag ON flag.round_id=round.id ` +
      `WHERE round.game_id=game.id AND round.finalized=false),` +
      `'verifiedFlags',(SELECT count(*) FROM "AdRounds" round ` +
      `JOIN "AdCheckResults" result ON result.round_id=round.id ` +
      `WHERE round.game_id=game.id AND round.finalized=false AND result.status=0 ` +
      `AND result.flag_verified=true AND result.sla_credit IS NOT NULL),` +
      `'rosterServices',(SELECT count(*) FROM "AdRounds" round ` +
      `JOIN "AdFlags" flag ON flag.round_id=round.id ` +
      `WHERE round.game_id=game.id AND round.number=game.ad_scoring_start_round)` +
      `)::text FROM "Games" game WHERE game.id=${gameId}`
  );
  if (!row) throw new Error(`A&D game ${gid} does not exist`);
  return JSON.parse(row);
}

function validateReadinessArguments(kind, expectedTeams, timeoutSeconds) {
  const teamCount = Number(expectedTeams);
  if (!Number.isSafeInteger(teamCount) || teamCount < 2) {
    throw new Error(`${kind} readiness requires at least two expected teams (got ${expectedTeams})`);
  }
  if (!Number.isSafeInteger(timeoutSeconds) || timeoutSeconds < 0) {
    throw new Error(`${kind} readiness timeout must be a non-negative integer (got ${timeoutSeconds})`);
  }
  return teamCount;
}

function epochBoundaryReady(snapshot, requiredTeams) {
  return (
    snapshot.startRound !== null &&
    snapshot.checkerCount > 0 &&
    snapshot.preparedCheckerCount === snapshot.checkerCount &&
    snapshot.rosterTeams === requiredTeams &&
    snapshot.rosterServices === requiredTeams * snapshot.checkerCount &&
    snapshot.liveRound >= snapshot.startRound &&
    snapshot.flagsPublished === true &&
    snapshot.plantedFlags === snapshot.rosterServices
  );
}

async function waitForEpochState(gid, expectedTeams, timeoutSeconds, requireExactEvidence) {
  const requiredTeams = validateReadinessArguments('epoch', expectedTeams, timeoutSeconds);
  let snapshot = null;
  for (let waited = 0; waited <= timeoutSeconds; waited++) {
    snapshot = epochReadiness(gid);
    const ready =
      epochBoundaryReady(snapshot, requiredTeams) &&
      (!requireExactEvidence || snapshot.verifiedFlags === snapshot.rosterServices);
    if (ready) return snapshot;
    if (waited < timeoutSeconds) await sleep(1000);
  }
  throw new Error(
    `official epoch ${requireExactEvidence ? 'exact evidence' : 'boundary'} did not become ready within ` +
      `${timeoutSeconds}s; wanted ${requiredTeams} frozen teams, observed ${JSON.stringify(snapshot)}`
  );
}

/** Wait only for scheduler-owned roster and a settled current publication pipeline. */
export async function waitForEpochBoundary(
  gid,
  expectedTeams,
  timeoutSeconds = Number(process.env.EPOCH_READY_TIMEOUT_SECONDS || 360)
) {
  return waitForEpochState(gid, expectedTeams, timeoutSeconds, false);
}

/** Compatibility contract: require exact current evidence for the complete roster. */
export async function waitForEpochReady(
  gid,
  expectedTeams,
  timeoutSeconds = Number(process.env.EPOCH_READY_TIMEOUT_SECONDS || 360)
) {
  return waitForEpochState(gid, expectedTeams, timeoutSeconds, true);
}

/** Materialize any missing finalized epoch rollups before timed load begins. */
export async function warmEpochBoard(gid, expectedTeams) {
  const started = performance.now();
  const response = await api('GET', `/api/Game/${Number(gid)}/Ad/Scoreboard`, {
    jwt: adminJwt(),
    ip: '10.9.9.10',
  });
  const model = response.json;
  if (
    response.status !== 200 ||
    model?.started !== true ||
    model?.startRound == null ||
    !Array.isArray(model?.teams) ||
    model.teams.length !== Number(expectedTeams)
  ) {
    throw new Error(`official epoch scoreboard warmup failed: ${response.status} ${response.text?.slice(0, 300)}`);
  }
  return Math.round(performance.now() - started);
}

// ── Bulk cohort seed (SQL) ────────────────────────────────────────────────────
/** Seed n single-member teams already Accepted into game gid. Returns {userIds, teamIds, partIds}. */
const keepId = (l) => l && !/^(INSERT|UPDATE|DELETE|COPY|SELECT)\s+\d+/.test(l);

export function seedCohort(gid, n) {
  // One database statement prevents a concurrent namespace sweep from observing
  // users or teams before their game-owned participation links exist.
  return parseCohortSeedResult(sql(cohortSeedQuery(gid, n)), n);
}

/** One real, checker-reachable AdTeamService per participation. */
export function seedAdServices(gid, cid, host, port) {
  const servicePort = Number(port);
  if (!host || !Number.isSafeInteger(servicePort) || servicePort < 1 || servicePort > 65535) {
    throw new Error(`invalid seeded A&D endpoint ${host}:${port}`);
  }
  const serviceHost = String(host).replaceAll("'", "''");
  sql(
    `INSERT INTO "AdTeamServices"(game_id,participation_id,challenge_id,host,port,status)
       SELECT ${gid}, p.id, ${cid}, '${serviceHost}', ${servicePort}, 3
       FROM "Participations" p WHERE p.game_id=${gid}`
  );
}

/** Restore the deterministic exact-checker fixture after an earlier canary teardown. */
export function restoreSeededAdServices(gid, cid, endpoint) {
  const gameId = Number(gid);
  const challengeId = Number(cid);
  const match = String(endpoint).match(/^([A-Za-z0-9._-]+):(\d{1,5})$/);
  const port = match ? Number(match[2]) : 0;
  if (
    !Number.isSafeInteger(gameId) ||
    gameId <= 0 ||
    !Number.isSafeInteger(challengeId) ||
    challengeId <= 0 ||
    !match ||
    port < 1 ||
    port > 65_535
  ) {
    throw new Error(`invalid seeded A&D service restore ${gid}/${cid}/${endpoint}`);
  }
  const host = match[1].replaceAll("'", "''");
  const replacedContainers = (
    sql(
      `SELECT DISTINCT container_id FROM "AdTeamServices" ` +
        `WHERE game_id=${gameId} AND challenge_id=${challengeId} AND container_id IS NOT NULL`
    ) || ''
  )
    .split('\n')
    .filter((id) => /^[a-f0-9]{64}$/.test(id));
  for (const containerId of replacedContainers) docker(['rm', '-f', containerId]);
  sql(
    `UPDATE "AdTeamServices" SET host='${host}',port=${port},status=3,container_id=NULL,last_reset_at=NULL ` +
      `WHERE game_id=${gameId} AND challenge_id=${challengeId}`
  );
}
export function seedKothTarget(gid, cid) {
  sql(`INSERT INTO "KothTargets"(game_id,challenge_id,host,port) VALUES (${gid},${cid},'10.66.0.1',9999)
       ON CONFLICT DO NOTHING`);
}

// ── State file ────────────────────────────────────────────────────────────────
export function writeState(obj) {
  const temporary = `${STATE}.${process.pid}.tmp`;
  try {
    writeFileSync(temporary, JSON.stringify(obj, null, 2), { mode: 0o600 });
    chmodSync(temporary, 0o600);
    renameSync(temporary, STATE);
  } finally {
    rmSync(temporary, { force: true });
  }
}
export function readState() {
  return existsSync(STATE) ? JSON.parse(readFileSync(STATE, 'utf8')) : null;
}
export const stateFile = STATE;

function retainedManifestGameIds() {
  const retained = new Set();
  for (const filename of readdirSync(STATE_DIRECTORY).filter((name) => STATE_MANIFEST_PATTERN.test(name))) {
    let manifest;
    try {
      manifest = JSON.parse(readFileSync(`${STATE_DIRECTORY}${filename}`, 'utf8'));
    } catch (error) {
      throw new Error(`cannot verify retained lifecycle manifest ${filename}: ${error.message}`);
    }
    if (manifest?.retained !== true) continue;

    // A database replacement can restart the Games sequence. Bind protection
    // to the exact load-test identity instead of allowing a historical numeric
    // id to protect an unrelated game created by the replacement cluster.
    for (const gameId of manifest.gameIds || []) {
      const id = Number(gameId);
      if (!Number.isSafeInteger(id) || id <= 0) continue;
      const currentTitle = sql(`SELECT title FROM "Games" WHERE id=${id}`);
      if (retainedManifestMatchesGame(manifest, id, currentTitle, filename)) retained.add(id);
    }
  }
  return retained;
}

// ── Namespaced teardown (never touches games 9/10) ────────────────────────────
function kothRuntimeScope(gameIds) {
  const inList = gameIds.join(',');
  const targetContainerIds = (
    sql(
      `SELECT container_id FROM "KothTargets" ` +
        `WHERE game_id IN (${inList}) AND NULLIF(BTRIM(container_id),'') IS NOT NULL`
    ) || ''
  )
    .split('\n')
    .filter((id) => /^[a-f0-9]{12,64}$/.test(id));
  const cycleIds = new Set(
    (sql(`SELECT id FROM "KothCrownCycles" WHERE game_id IN (${inList})`) || '')
      .split('\n')
      .filter((id) => /^\d+$/.test(id))
  );
  return { targetContainerIds, cycleIds };
}

let cachedDockerScope;

function currentDockerScope() {
  if (cachedDockerScope) return cachedDockerScope;
  const inspected = mustDocker(
    docker(['inspect', RSCTF, '--format', '{{json .Config.Env}}']),
    'discover rsctf Docker workload scope',
  );
  const environment = JSON.parse(inspected.stdout.trim() || '[]');
  cachedDockerScope = dockerScopeFromContainerEnv(environment);
  return cachedDockerScope;
}

function kothOperationContainerIds(cycleIds) {
  if (cycleIds.size === 0) return [];
  const scope = currentDockerScope();
  const runtimes = mustDocker(
    docker([
      'ps',
      '-a',
      '--filter',
      `label=rsctf.managed=${scope}`,
      '--filter',
      `label=rsctf.scope=${scope}`,
      '--filter',
      'label=rsctf.operation',
      '--format',
      '{{.ID}}\t{{.Label "rsctf.operation"}}\t{{.Label "rsctf.scope"}}',
    ]),
    'discover KotH reset-operation containers'
  );
  return runtimes.stdout
    .trim()
    .split('\n')
    .filter(Boolean)
    .map((row) => row.split('\t'))
    .filter(
      ([, operation, runtimeScope]) =>
        runtimeScope === scope &&
        cycleIds.has(String(managedKothOperationCycleId(operation))),
    )
    .map(([id]) => id)
    .filter((id) => /^[a-f0-9]{12,64}$/.test(id));
}

export function kothContainerIdsForGames(gameIds) {
  const ids = [...new Set(gameIds.map(Number))];
  if (ids.length === 0) return [];
  if (ids.some((gameId) => !Number.isSafeInteger(gameId) || gameId <= 0)) {
    throw new Error('KotH container discovery requires one or more valid game ids');
  }
  const scope = kothRuntimeScope(ids);
  return [...new Set([...scope.targetContainerIds, ...kothOperationContainerIds(scope.cycleIds)])];
}

function removeManagedKothContainers(containerIds) {
  for (const containerId of new Set(containerIds)) {
    const removal = docker(['rm', '-f', containerId]);
    if (removal.status !== 0 && !/no such (?:container|object)/i.test(removal.stderr)) {
      throw new Error(`remove managed KotH container ${containerId}: ${removal.stderr.trim()}`);
    }
  }
}

export async function teardownNamespace(gameIds) {
  const ids = [...new Set(gameIds.map(Number))];
  if (ids.length === 0 || ids.some((gameId) => !Number.isSafeInteger(gameId) || gameId <= 0)) {
    throw new Error('teardown requires one or more valid load-test game ids');
  }
  const retainedIds = retainedManifestGameIds();
  const targetsRetainedEvent = ids.some((gameId) => retainedIds.has(gameId));
  if (targetsRetainedEvent && process.env.DELETE_RETAINED_EVENT !== '1') {
    throw new Error('refusing to delete retained simulation evidence; set DELETE_RETAINED_EVENT=1 explicitly');
  }
  // Preserve every durable runtime identity before attempting the public path.
  // A failed API delete must not erase the only way a retry can find a hill.
  const kothScope = kothRuntimeScope(ids);
  const managedEventContainers = new Set([
    ...kothScope.targetContainerIds,
    ...kothOperationContainerIds(kothScope.cycleIds),
  ]);
  const { disposableLoadGameRuntimeIds } = await import('./admin-fixtures.mjs');
  for (const gameId of ids) {
    const title = sql(`SELECT title FROM "Games" WHERE id=${gameId}`);
    if (!title) continue;
    if (!LOAD_GAME_TITLE_RE.test(title)) {
      throw new Error(`teardown blocked for non-load game ${gameId}`);
    }
    for (const containerId of disposableLoadGameRuntimeIds(gameId, title)) {
      managedEventContainers.add(containerId);
    }
  }

  // First use the public path so live containers and capabilities are revoked. A
  // concurrent scheduler/checker can make this return a transient 5xx; the exact
  // namespace cleanup below is idempotent, then the public delete is retried and
  // verified instead of silently leaving an empty load-test game behind.
  for (const g of ids) {
    let response = await deleteGameWithRetry(g);
    if (response.status < 300) continue;
    const title = sql(`SELECT title FROM "Games" WHERE id=${g}`);
    if (!title) continue;
    if (!LOAD_GAME_TITLE_RE.test(title)) {
      throw new Error(`teardown blocked for non-load game ${g}: ${response.text}`);
    }
    const runtimeIds = [...managedEventContainers].filter((containerId) => containerId);
    removeManagedKothContainers(runtimeIds);
    mustDocker(
      docker(['exec', RSCTF, 'rm', '-rf', `/data/files/checkers/load/${g}`]),
      `remove checker directory for game ${g}`,
    );
    if (response.status !== 400 || !isDeletionProtectedByEvidence(response)) {
      throw new Error(`teardown unexpected delete status ${response.status} for game ${g}: ${response.text}`);
    }
    await exactProtectedLoadGameCleanup(g, title, runtimeIds);
    if (Number(sql(`SELECT count(*) FROM "Games" WHERE id=${g}`)) !== 0) {
      throw new Error(`teardown fallback did not remove game ${g}`);
    }
  }
  for (const containerId of kothOperationContainerIds(kothScope.cycleIds)) {
    managedEventContainers.add(containerId);
  }
  removeManagedKothContainers(managedEventContainers);
  for (const g of ids) {
    if (Number(sql(`SELECT count(*) FROM "Games" WHERE id=${g}`)) === 0) {
      continue;
    }
    const title = sql(`SELECT title FROM "Games" WHERE id=${g}`);
    if (!title) continue;
    const response = await deleteGameWithRetry(g);
    if (response.status < 300) continue;
    if (response.status === 400 && isDeletionProtectedByEvidence(response) && LOAD_GAME_TITLE_RE.test(title)) {
      removeManagedKothContainers(managedEventContainers);
      mustDocker(
        docker(['exec', RSCTF, 'rm', '-rf', `/data/files/checkers/load/${g}`]),
        `remove checker directory for game ${g}`,
      );
      await exactProtectedLoadGameCleanup(g, title, [...managedEventContainers]);
      continue;
    }
    throw new Error(`teardownNamespace cleanup failed for game ${g}: ${response.status} ${response.text}`);
  }
  for (const g of ids) {
    if (Number(sql(`SELECT count(*) FROM "Games" WHERE id=${g}`)) !== 0) {
      throw new Error(`teardown left ${g} load-game row`);
    }
  }
}

// ── Attachments (upload a real local file → serve it at /assets/{hash}) ─────────
export async function uploadAsset(filename, content) {
  const fd = new FormData();
  fd.append('file', new Blob([content]), filename);
  const operationId = randomUUID();
  const r = await fetch(`${TARGET}/api/assets?operationId=${operationId}`, {
    method: 'POST',
    headers: { authorization: `Bearer ${adminJwt()}`, 'x-real-ip': '10.9.9.9' },
    body: fd,
  });
  const j = await r.json().catch(() => null);
  const uploaded = Array.isArray(j) ? j[0] : j;
  if (r.status >= 300 || !uploaded?.hash || !uploaded?.uploadId) {
    throw new Error(`uploadAsset → ${r.status} ${JSON.stringify(j)?.slice(0, 120)}`);
  }
  return { hash: uploaded.hash, uploadId: uploaded.uploadId };
}
export async function setAttachment(gid, cid, uploaded) {
  return must(
    await api('POST', `/api/edit/games/${gid}/challenges/${cid}/attachment`, {
      ...jwtOpt(),
      body: { attachmentType: 'Local', fileHash: uploaded.hash, uploadId: uploaded.uploadId },
    }),
    'setAttachment'
  );
}

// ── Real BYOC fleet: agents for ACTUAL seeded participations (real tunnels) ─────
let activeFleetScope = null;

function mustDocker(result, what) {
  if (result.status !== 0) {
    throw new Error(`${what}: ${(result.stderr || result.error?.message || 'docker command failed').trim()}`);
  }
  return result;
}

function inspectContainer(reference) {
  const result = docker(['inspect', reference]);
  if (result.status !== 0) {
    if (/no such (?:container|object)/i.test(result.stderr)) return null;
    throw new Error(`inspect lifecycle container ${reference}: ${result.stderr.trim()}`);
  }
  const records = JSON.parse(result.stdout);
  return records[0] || null;
}

function inspectVolume(name) {
  const result = docker(['volume', 'inspect', name]);
  if (result.status !== 0) {
    if (/no such volume/i.test(result.stderr)) return null;
    throw new Error(`inspect lifecycle volume ${name}: ${result.stderr.trim()}`);
  }
  const records = JSON.parse(result.stdout);
  return records[0] || null;
}

function inspectContainerReferences(references, what) {
  const records = [];
  for (let offset = 0; offset < references.length; offset += 32) {
    const batch = references.slice(offset, offset + 32);
    const result = mustDocker(docker(['inspect', ...batch]), what);
    records.push(...JSON.parse(result.stdout));
  }
  return records;
}

function inspectVolumeReferences(references, what) {
  const records = [];
  for (let offset = 0; offset < references.length; offset += 32) {
    const batch = references.slice(offset, offset + 32);
    const result = mustDocker(docker(['volume', 'inspect', ...batch]), what);
    records.push(...JSON.parse(result.stdout));
  }
  return records;
}

function assertOwnedResource(labels, scope, role, participationId, description) {
  if (!ownsFleetResource(labels, scope, role, participationId)) {
    throw new Error(
      `refusing to replace unowned ${description}; remove it explicitly or restore its lifecycle ownership labels`
    );
  }
}

function removeOwnedNamedContainer(name, scope, role, participationId = null) {
  const container = inspectContainer(name);
  if (!container) return;
  assertOwnedResource(container.Config?.Labels, scope, role, participationId, `container ${name}`);
  const removal = docker(['rm', '-f', container.Id]);
  if (removal.status !== 0 && !/no such (?:container|object)/i.test(removal.stderr)) {
    throw new Error(`remove lifecycle container ${name}: ${removal.stderr.trim()}`);
  }
}

function removeOwnedNamedVolume(name, scope, participationId) {
  const volume = inspectVolume(name);
  if (!volume) return;
  assertOwnedResource(volume.Labels, scope, 'flag-volume', participationId, `volume ${name}`);
  const removal = docker(['volume', 'rm', '-f', name]);
  if (removal.status !== 0 && !/no such volume/i.test(removal.stderr)) {
    throw new Error(`remove lifecycle volume ${name}: ${removal.stderr.trim()}`);
  }
}

function ownedContainerIds(scope, runningOnly = false) {
  const result = mustDocker(
    docker(['ps', runningOnly ? '-q' : '-aq', ...dockerOwnershipFilterArgs(scope)]),
    'discover owned lifecycle BYOC containers'
  );
  return result.stdout
    .trim()
    .split('\n')
    .filter((id) => /^[a-f0-9]{12,64}$/.test(id));
}

function ownedVolumeNames(scope) {
  const result = mustDocker(
    docker(['volume', 'ls', '-q', ...dockerOwnershipFilterArgs(scope)]),
    'discover owned lifecycle BYOC volumes'
  );
  return result.stdout
    .trim()
    .split('\n')
    .filter(Boolean);
}

function inspectOwnedContainers(scope, runningOnly = false) {
  const ids = ownedContainerIds(scope, runningOnly);
  return inspectContainerReferences(ids, 'inspect owned lifecycle BYOC containers');
}

function inspectOwnedVolumes(scope) {
  const names = ownedVolumeNames(scope);
  return inspectVolumeReferences(names, 'inspect owned lifecycle BYOC volumes');
}

/** Install a dependency-free checker into the same persistent tree as git-sync checkers. */
function prepareChecker(gid, cid, source, label) {
  const dest = `/data/files/checkers/load/${Number(gid)}/${Number(cid)}`;
  mustDocker(docker(['exec', RSCTF, 'rm', '-rf', dest]), `remove stale lifecycle ${label} checker`);
  mustDocker(docker(['exec', RSCTF, 'mkdir', '-p', `${dest}/src`]), `create lifecycle ${label} checker directory`);
  mustDocker(docker(['cp', source, `${RSCTF}:${dest}/src/run.py`]), `copy lifecycle ${label} checker`);
  mustDocker(
    docker(['exec', RSCTF, 'python3', '-m', 'venv', `${dest}/venv`]),
    `prepare lifecycle ${label} checker venv`
  );
  mustDocker(docker(['exec', RSCTF, 'chmod', '-R', 'a+rX', dest]), `make lifecycle ${label} checker sandbox-readable`);
  mustDocker(
    docker(['exec', RSCTF, 'test', '-x', `${dest}/venv/bin/python3`]),
    `verify lifecycle ${label} checker interpreter`
  );
  mustDocker(
    docker(['exec', RSCTF, 'test', '-f', `${dest}/src/run.py`]),
    `verify lifecycle ${label} checker entrypoint`
  );
  return dest;
}
export function prepareExactChecker(gid, cid) {
  return prepareChecker(gid, cid, materializeFixtures().checker, 'A&D');
}
export function prepareKothChecker(gid, cid) {
  return prepareChecker(gid, cid, materializeFixtures().kothChecker, 'KotH');
}

function buildManagedFixtureImage(tag, dockerfile, label) {
  const baseReference = process.env.LOAD_FIXTURE_PYTHON_IMAGE ||
    'python:3.12-alpine@sha256:d09d15e60962ca365d1cd544a48773bac9d33f2fb1b00f2aa0deec78ade7dc31';
  if (!isImmutableImageReference(baseReference)) {
    throw new Error('LOAD_FIXTURE_PYTHON_IMAGE must be an immutable repository digest or image ID');
  }
  let inspected = docker(['image', 'inspect', baseReference, '--format', '{{.Id}}']);
  if (inspected.status !== 0) {
    mustDocker(docker(['pull', baseReference]), `pull ${label} base image`);
    inspected = docker(['image', 'inspect', baseReference, '--format', '{{.Id}}']);
  }
  const baseImageId = mustDocker(inspected, `inspect ${label} base image`).stdout.trim();
  if (!/^sha256:[a-f0-9]{64}$/.test(baseImageId)) {
    throw new Error(`${label} base did not resolve to an immutable Docker image ID`);
  }
  const fixtures = materializeFixtures();
  mustDocker(
    docker([
      'build',
      '--pull=false',
      '--tag',
      tag,
      '--file',
      dockerfile(fixtures),
      '--build-arg',
      `BASE_IMAGE=${baseReference}`,
      fixtures.root,
    ]),
    `build ${label} image`
  );
  const identity = mustDocker(
    docker(['image', 'inspect', tag, '--format', '{{.Id}}']),
    `inspect ${label} image`
  ).stdout.trim();
  if (!isImmutableImageReference(identity) || !identity.startsWith('sha256:')) {
    throw new Error(`${label} did not produce an immutable Docker image ID`);
  }
  return identity;
}

export function buildManagedAdImage() {
  return buildManagedFixtureImage(
    'rsctf-load-ad:functional-v1',
    (fixtures) => fixtures.adDockerfile,
    'functional A&D fixture',
  );
}

export function buildCompetitiveKothImage() {
  return buildManagedFixtureImage(
    'rsctf-load-koth:competitive-v1',
    (fixtures) => fixtures.kothDockerfile,
    'competitive KotH fixture',
  );
}

export function buildManagedKothImage() {
  return buildManagedFixtureImage(
    'rsctf-load-koth:managed-v1',
    (fixtures) => fixtures.managedKothDockerfile,
    'managed Leaderboard KotH fixture',
  );
}

export function startFleetService(gameId, cid) {
  const scope = normalizeFleetScope(gameId, cid);
  const existing = inspectContainer('lcbyoc_svc');
  if (existing) {
    assertOwnedResource(existing.Config?.Labels, scope, 'shared-service', null, 'container lcbyoc_svc');
    const networks = existing.NetworkSettings?.Networks || {};
    const checkerAliases = networks[AD_NET]?.Aliases || [];
    const tunnelAliases = networks[NET]?.Aliases || [];
    if (checkerAliases.includes('lcbyoc_checker_svc') && tunnelAliases.includes('lcbyoc_tunnel_svc')) {
      return {
        checker: 'lcbyoc_checker_svc:8080',
        tunnel: 'lcbyoc_tunnel_svc:8080',
      };
    }
    removeOwnedNamedContainer('lcbyoc_svc', scope, 'shared-service');
  }
  const image = mustDocker(
    docker(['inspect', RSCTF, '--format', '{{.Config.Image}}']),
    'discover rsctf image for lifecycle service'
  ).stdout.trim();
  const source = materializeFixtures().service;
  mustDocker(
    docker([
      'run',
      '-d',
      '--rm',
      '--name',
      'lcbyoc_svc',
      ...BOUNDED_DOCKER_LOG_ARGS,
      ...dockerLabelArgs(fleetLabels(scope, 'shared-service')),
      '--network',
      AD_NET,
      '--network-alias',
      'lcbyoc_checker_svc',
      '--entrypoint',
      'python3',
      '-e',
      'PORT=8080',
      '-v',
      `${source}:/opt/load-ad-service.py:ro`,
      image,
      '/opt/load-ad-service.py',
    ]),
    'start lifecycle exact-flag service'
  );
  mustDocker(
    docker(['network', 'connect', '--alias', 'lcbyoc_tunnel_svc', NET, 'lcbyoc_svc']),
    'connect lifecycle service to the BYOC agent network'
  );
  return {
    checker: 'lcbyoc_checker_svc:8080',
    tunnel: 'lcbyoc_tunnel_svc:8080',
  };
}

function defenseKeyFor(_gameId, capability) {
  // Domain-separate the already-random per-team BYOC capability. Public game,
  // challenge, and participation ids are insufficient to derive another
  // team's defense control, and the BYOC bearer itself is never reused here.
  return `ld_${createHash('sha256')
    .update('rsctf-load-defense\0')
    .update(capability.token)
    .digest('base64url')}`;
}

function registerFleetCapabilities(capabilities, gameId) {
  const cid = capabilities[0]?.cid;
  activeFleetScope = normalizeFleetScope(
    gameId,
    cid,
    capabilities.map(({ pid }) => pid)
  );
  return capabilities.map((capability) => ({
    ...capability,
    defenseKey: process.env.LIFECYCLE_ISOLATED_SERVICES === '1' ? defenseKeyFor(gameId, capability) : null,
  }));
}

/** Adopt an already-running provision fleet so lifecycle cleanup retains ownership. */
export function adoptFleetForPids(gameId, cid, pids) {
  const capabilities = byocCapabilitiesForPids(pids, cid, gameId);
  if (!fleetResourcesReady(gameId, cid, pids, process.env.LIFECYCLE_ISOLATED_SERVICES === '1')) {
    throw new Error('cannot adopt a BYOC fleet whose exact labeled resources are missing or inconsistent');
  }
  return registerFleetCapabilities(capabilities, gameId);
}

function startIsolatedFleetServices(capabilities, gameId) {
  if (process.env.LIFECYCLE_ISOLATED_SERVICES !== '1') return null;

  const scope = normalizeFleetScope(
    gameId,
    capabilities[0]?.cid,
    capabilities.map(({ pid }) => pid)
  );

  const image = mustDocker(
    docker(['inspect', RSCTF, '--format', '{{.Config.Image}}']),
    'discover rsctf image for isolated lifecycle services'
  ).stdout.trim();
  const source = materializeFixtures().service;
  return capabilities.map((capability, i) => {
    const name = `lcbyoc_svc_${i}`;
    const flagVolume = `lcbyoc_flag_${i}`;
    const defenseKey = defenseKeyFor(gameId, capability);
    // A prior interrupted lifecycle run may have left this namespaced container
    // behind. Removing the exact deterministic name makes retries idempotent.
    removeOwnedNamedContainer(name, scope, 'isolated-service', capability.pid);
    removeOwnedNamedVolume(flagVolume, scope, capability.pid);
    mustDocker(
      docker(['volume', 'create', ...dockerLabelArgs(fleetLabels(scope, 'flag-volume', capability.pid)), flagVolume]),
      `create isolated lifecycle flag volume ${i}`
    );
    mustDocker(
      docker([
        'run',
        '-d',
        '--rm',
        '--name',
        name,
        ...BOUNDED_DOCKER_LOG_ARGS,
        ...dockerLabelArgs(fleetLabels(scope, 'isolated-service', capability.pid)),
        '--network',
        NET,
        '--entrypoint',
        'python3',
        '-e',
        'PORT=8080',
        '-e',
        'FLAG_FILE=/shared/flag',
        '-e',
        `DEFENSE_KEY=${defenseKey}`,
        '-v',
        `${source}:/opt/load-ad-service.py:ro`,
        '-v',
        `${flagVolume}:/shared:ro`,
        image,
        '/opt/load-ad-service.py',
      ]),
      `start isolated lifecycle service ${i}`
    );
    return { address: `${name}:8080`, flagVolume, defenseKey };
  });
}

export function startFleetForPids(gameId, cid, pids, svcAddr) {
  const capabilities = byocCapabilitiesForPids(pids, cid, gameId);
  registerFleetCapabilities(capabilities, gameId);
  const scope = activeFleetScope;
  sql(
    `UPDATE "AdTeamServices" SET host='',port=0,status=2 WHERE game_id=${gameId} AND challenge_id=${cid} ` +
      `AND participation_id IN (${capabilities.map(({ pid }) => pid).join(',')})`
  );
  const ip = byocRsctfIp();
  try {
    const isolatedServices = startIsolatedFleetServices(capabilities, gameId);
    capabilities.forEach((capability, i) => {
      const url = `ws://${ip}:8080/api/Game/${gameId}/Ad/Byoc/Agent/${capability.pid}/${cid}/${capability.token}`;
      const isolated = isolatedServices?.[i];
      const service = isolated?.address || svcAddr;
      removeOwnedNamedContainer(`lcbyoc_${i}`, scope, 'relay', capability.pid);
      const result = docker([
        'run',
        '-d',
        '--rm',
        '--name',
        `lcbyoc_${i}`,
        ...BOUNDED_DOCKER_LOG_ARGS,
        ...dockerLabelArgs(fleetLabels(scope, 'relay', capability.pid)),
        '--network',
        NET,
        ...(isolated ? ['-v', `${isolated.flagVolume}:/shared`] : []),
        '-e',
        'RSCTF_BYOC_MODE=agent',
        '-e',
        `RSCTF_BYOC_TUNNEL_URL=${url}`,
        '-e',
        `RSCTF_BYOC_SERVICE=${service}`,
        ...(isolated
          ? [
              '-e',
              'RSCTF_BYOC_FLAG_FILE=/shared/flag',
            ]
          : []),
        byocAgentImage(),
      ]);
      if (result.status !== 0) {
        throw new Error(
          `failed to start lifecycle BYOC agent for participation ${capability.pid}: ${result.stderr.trim()}`
        );
      }
    });
  } catch (error) {
    teardownFleet();
    throw error;
  }
  return registerFleetCapabilities(capabilities, gameId);
}
export function tunnelsUpFor(gameId, cid, pids) {
  if (!pids.length) return 0;
  return Number(
    sql(
      `SELECT count(DISTINCT participation_id) FROM "AdTeamServices" WHERE game_id=${gameId} ` +
        `AND challenge_id=${cid} AND participation_id IN (${pids.join(',')}) AND port>0`
    )
  );
}

/** Wait until every requested participation owns a live tunnel endpoint. */
export async function waitForFleetReady(gameId, cid, pids, timeoutSeconds = 40) {
  const dynamicTimeout = Number(
    process.env.BYOC_FLEET_READY_TIMEOUT_SECONDS || timeoutSeconds
  );
  if (!Number.isSafeInteger(dynamicTimeout) || dynamicTimeout < 0) {
    throw new Error(
      `invalid BYOC_FLEET_READY_TIMEOUT_SECONDS (got ${process.env.BYOC_FLEET_READY_TIMEOUT_SECONDS})`
    );
  }
  const gid = Number(gameId);
  const challengeId = Number(cid);
  const participationIds = Array.isArray(pids) ? pids.map(Number) : [];
  if (
    !Number.isSafeInteger(gid) ||
    gid <= 0 ||
    !Number.isSafeInteger(challengeId) ||
    challengeId <= 0 ||
    participationIds.length === 0 ||
    new Set(participationIds).size !== participationIds.length ||
    participationIds.some((pid) => !Number.isSafeInteger(pid) || pid <= 0) ||
    !Number.isSafeInteger(timeoutSeconds) ||
    timeoutSeconds < 0
  ) {
    throw new Error(
      'fleet readiness requires valid game/challenge ids, distinct participations, and an integer timeout'
    );
  }
  let up = 0;
  for (let waited = 0; waited <= dynamicTimeout; waited++) {
    up = tunnelsUpFor(gid, challengeId, participationIds);
    if (up === participationIds.length) return up;
    if (waited < dynamicTimeout) await sleep(1000);
  }
  throw new Error(
    `BYOC fleet did not become ready (${up}/${participationIds.length} tunnels) after ${dynamicTimeout}s`
  );
}

/** Exact delivery/checker evidence for the selected fleet's current round. */
export function fleetExactReadiness(gameId, cid, pids) {
  const gid = Number(gameId);
  const challengeId = Number(cid);
  const participationIds = Array.isArray(pids) ? pids.map(Number) : [];
  if (
    !Number.isSafeInteger(gid) ||
    gid <= 0 ||
    !Number.isSafeInteger(challengeId) ||
    challengeId <= 0 ||
    participationIds.length === 0 ||
    new Set(participationIds).size !== participationIds.length ||
    participationIds.some((pid) => !Number.isSafeInteger(pid) || pid <= 0)
  ) {
    throw new Error('exact fleet evidence requires valid game/challenge ids and distinct participations');
  }
  const requested = participationIds.join(',');
  const row = sql(
    `WITH current_round AS (` +
      `SELECT id,number FROM "AdRounds" WHERE game_id=${gid} AND finalized=false ` +
      `ORDER BY number DESC LIMIT 1` +
      `), requested(participation_id) AS (` +
      `SELECT unnest(ARRAY[${requested}]::integer[])` +
      `), services AS (` +
      `SELECT service.id FROM requested ` +
      `JOIN "AdTeamServices" service ON service.participation_id=requested.participation_id ` +
      `AND service.game_id=${gid} AND service.challenge_id=${challengeId}` +
      `) SELECT json_build_object(` +
      `'liveRound',(SELECT number FROM current_round),` +
      `'requestedServices',(SELECT count(*) FROM services),` +
      `'plantedFlags',(SELECT count(*) FROM current_round round ` +
      `JOIN "AdFlags" flag ON flag.round_id=round.id JOIN services ON services.id=flag.team_service_id),` +
      `'deliveredFlags',(SELECT count(*) FROM current_round round ` +
      `JOIN "AdFlagDeliveryResults" delivery ON delivery.round_id=round.id ` +
      `JOIN services ON services.id=delivery.team_service_id WHERE delivery.delivered=true),` +
      `'verifiedFlags',(SELECT count(*) FROM current_round round ` +
      `JOIN "AdCheckResults" result ON result.round_id=round.id ` +
      `JOIN services ON services.id=result.team_service_id WHERE result.status=0 ` +
      `AND result.flag_verified=true AND result.sla_credit IS NOT NULL)` +
      `)::text`
  );
  return JSON.parse(row);
}

/** Wait for a post-connect round with exact evidence for only the selected fleet. */
export async function waitForFleetExactEvidence(
  gameId,
  cid,
  pids,
  {
    afterRound = 0,
    allowCurrentRoundEvidence = false,
    timeoutSeconds = Number(process.env.EPOCH_READY_TIMEOUT_SECONDS || 360),
  } = {}
) {
  const requiredServices = Array.isArray(pids) ? pids.length : 0;
  const canReuseCurrentRound =
    allowCurrentRoundEvidence && afterRound > 0;
  if (
    !Number.isSafeInteger(afterRound) ||
    afterRound < 0 ||
    !Number.isSafeInteger(timeoutSeconds) ||
    timeoutSeconds < 0
  ) {
    throw new Error('exact fleet evidence requires non-negative integer round and timeout values');
  }
  let snapshot = null;
  for (let waited = 0; waited <= timeoutSeconds; waited++) {
    snapshot = fleetExactReadiness(gameId, cid, pids);
    const ready =
      (canReuseCurrentRound
        ? snapshot.liveRound >= afterRound
        : snapshot.liveRound > afterRound) &&
      snapshot.requestedServices === requiredServices &&
      snapshot.plantedFlags === requiredServices &&
      snapshot.deliveredFlags === requiredServices &&
      snapshot.verifiedFlags === requiredServices;
    if (ready) return snapshot;
    if (waited < timeoutSeconds) await sleep(1000);
  }
  throw new Error(
    `selected BYOC fleet did not produce exact evidence within ${timeoutSeconds}s; observed ${JSON.stringify(snapshot)}`
  );
}

/** Exact isolated-network listeners for the selected live lifecycle tunnels. */
export function tunnelListenersFor(gameId, cid, pids) {
  const gid = Number(gameId);
  const challengeId = Number(cid);
  const participationIds = Array.isArray(pids) ? [...new Set(pids.map(Number))] : [];
  if (
    !Number.isSafeInteger(gid) ||
    gid <= 0 ||
    !Number.isSafeInteger(challengeId) ||
    challengeId <= 0 ||
    !Array.isArray(pids) ||
    participationIds.some((pid) => !Number.isSafeInteger(pid) || pid <= 0)
  ) {
    throw new Error(`invalid lifecycle tunnel identity ${gameId}/${cid}`);
  }
  if (participationIds.length === 0) return [];

  const endpoints = sql(
    `SELECT service.host||':'||service.port FROM unnest(ARRAY[${participationIds.join(',')}]::int[]) WITH ORDINALITY requested(participation_id,position) ` +
      `JOIN "AdTeamServices" service ON service.participation_id=requested.participation_id ` +
      `AND service.game_id=${gid} AND service.challenge_id=${challengeId} ` +
      `WHERE service.host<>'' AND service.port>0 ORDER BY requested.position`
  );
  return (endpoints || '').split('\n').filter(keepId);
}

export function fleetResourcesReady(gameId, cid, pids, isolatedServices = false) {
  const scope = normalizeFleetScope(gameId, cid, pids);
  const containers = inspectOwnedContainers(scope, true);
  const expectedContainerCount = 1 + scope.participationIds.length * (isolatedServices ? 2 : 1);
  if (containers.length !== expectedContainerCount) return false;

  const hasContainer = (name, role, participationId = null) =>
    containers.some(
      (container) =>
        String(container.Name || '').replace(/^\//, '') === name &&
        ownsFleetResource(container.Config?.Labels, scope, role, participationId)
    );
  if (!hasContainer('lcbyoc_svc', 'shared-service')) return false;
  for (const [index, pid] of scope.participationIds.entries()) {
    if (!hasContainer(`lcbyoc_${index}`, 'relay', pid)) return false;
    if (isolatedServices && !hasContainer(`lcbyoc_svc_${index}`, 'isolated-service', pid)) return false;
  }

  const volumes = inspectOwnedVolumes(scope);
  if (volumes.length !== (isolatedServices ? scope.participationIds.length : 0)) return false;
  return (
    !isolatedServices ||
    scope.participationIds.every((pid, index) =>
      volumes.some(
        (volume) =>
          volume.Name === `lcbyoc_flag_${index}` &&
          ownsFleetResource(volume.Labels, scope, 'flag-volume', pid)
      )
    )
  );
}

function cleanupScopes(input) {
  if (!input && activeFleetScope) return [activeFleetScope];
  if (!input) return [];

  const gameId = input.gameId ?? input.mixGame;
  const challengeId = input.cid ?? input.challengeId ?? input.adChal;
  const participationIds = input.pids ?? input.participationIds ?? input.adPartIds ?? [];
  if (gameId != null) return [normalizeFleetScope(gameId, challengeId, participationIds)];

  if (!Array.isArray(input.gameIds)) throw new Error('fleet teardown requires an owned game scope');
  return [...new Set(input.gameIds.map(Number))].map((id) => normalizeFleetScope(id));
}

export function teardownFleet(input = null) {
  const scopes = cleanupScopes(input);
  for (const scope of scopes) {
    // Snapshot the exact label-bound identities before removal. SQL cleanup is
    // derived from these labels, not from a caller-supplied prefix of the fleet.
    const containers = inspectOwnedContainers(scope);
    const volumes = inspectOwnedVolumes(scope);
    const bindings = fleetParticipantBindings(scope, [
      ...containers.map((container) => ({ kind: 'container', labels: container.Config?.Labels })),
      ...volumes.map((volume) => ({ kind: 'volume', labels: volume.Labels })),
    ]);
    // Revoke every snapshotted participant endpoint before touching Docker.
    // If Docker removes only part of a batch or the process crashes, no removed
    // relay can retain a live database route and a retry remains idempotent.
    const bindingsByChallenge = new Map();
    for (const binding of bindings) {
      const challengeBindings = bindingsByChallenge.get(binding.challengeId) || [];
      challengeBindings.push(binding);
      bindingsByChallenge.set(binding.challengeId, challengeBindings);
    }
    for (const [challengeId, challengeBindings] of bindingsByChallenge) {
      sql(
        `UPDATE "AdTeamServices" SET host='',port=0,status=2 ` +
          `WHERE game_id=${scope.gameId} AND challenge_id=${challengeId} AND participation_id IN (` +
          `${challengeBindings.map(({ participationId }) => participationId).join(',')})`
      );
    }
    const containerIds = containers.map(({ Id }) => Id);
    if (containerIds.length) {
      const removal = docker(['rm', '-f', ...containerIds]);
      if (removal.status !== 0 && !/no such (?:container|object)/i.test(removal.stderr)) {
        throw new Error(`remove owned lifecycle BYOC containers: ${removal.stderr.trim()}`);
      }
    }
    const remainingContainers = ownedContainerIds(scope);
    if (remainingContainers.length) {
      throw new Error(
        `lifecycle BYOC teardown left ${remainingContainers.length} owned container(s): ` +
          remainingContainers.join(',')
      );
    }

    const flagVolumes = volumes.map(({ Name }) => Name);
    if (flagVolumes.length) {
      const removal = docker(['volume', 'rm', '-f', ...flagVolumes]);
      if (removal.status !== 0 && !/no such volume/i.test(removal.stderr)) {
        throw new Error(`remove owned lifecycle BYOC volumes: ${removal.stderr.trim()}`);
      }
    }
    const remainingVolumes = ownedVolumeNames(scope);
    if (remainingVolumes.length) {
      throw new Error(`lifecycle BYOC teardown left ${remainingVolumes.length} owned volume(s)`);
    }

  }
  activeFleetScope = null;
}

// ── KotH hill: a real container the checker reads /koth/king from, + capture write ─
export function startHill(gid, cid, image = 'nginx:alpine', port = 80) {
  const exposedPort = Number(port);
  if (!Number.isSafeInteger(exposedPort) || exposedPort < 1 || exposedPort > 65_535) {
    throw new Error(`invalid lifecycle KotH port ${port}`);
  }
  docker(['rm', '-f', 'lckoth_hill']);
  const ownershipLabels = dockerOwnershipLabelArgs(currentDockerScope());
  const out = mustDocker(
    docker([
      'run',
      '-d',
      '--rm',
      '--name',
      'lckoth_hill',
      '--network',
      NET,
      ...ownershipLabels,
      String(image),
    ]),
    'start lifecycle KotH hill'
  );
  const container = out.stdout.trim();
  if (!container) throw new Error('start lifecycle KotH hill returned no container id');
  sql(
    `UPDATE "KothTargets" SET container_id='${container}', host='lckoth_hill', port=${exposedPort} WHERE game_id=${gid} AND challenge_id=${cid}`
  );
  return container;
}
/** A team "captures" the hill by writing its current token to /koth/king (out-of-band, as real teams do). */
export function kothCaptureWrite(container, token) {
  if (!/^[a-f0-9]{12,64}$/i.test(String(container))) {
    throw new Error(`invalid KotH container identity ${container}`);
  }
  if (!/^koth_[A-Za-z0-9_-]{8,128}$/.test(String(token))) {
    throw new Error('invalid KotH capability shape');
  }
  mustDocker(
    docker(['exec', container, 'sh', '-c', `mkdir -p /koth && printf '%s' '${token}' > /koth/king`]),
    'write KotH capture marker'
  );
}

/** Select API claim input before the official snapshot and return its one-time secret. */
export async function configureKothApiObserver(gid, cid) {
  const metadata = unwrap(await must(
    await api('GET', `/api/edit/games/${Number(gid)}/ad/koth/${Number(cid)}/observer`, {
      ...jwtOpt(),
    }),
    'read KotH API referee revision',
  ));
  const response = await must(
    await api('POST', `/api/edit/games/${Number(gid)}/ad/koth/${Number(cid)}/observer`, {
      ...jwtOpt(),
      body: {
        operationId: randomUUID(),
        expectedRevision: Number(metadata.revision),
      },
    }),
    'configure KotH API referee',
  );
  const model = unwrap(response);
  if (
    model?.claimSource !== 'Api' ||
    model?.configured !== true ||
    typeof model?.secret !== 'string' ||
    !model.secret.startsWith('koth_api_')
  ) {
    throw new Error('configure KotH API referee returned an invalid one-time credential');
  }
  return model.secret;
}

let lastKothObservationTimestamp = 0;
const kothObservationLedgers = new Map();
const kothContextValidators = new Map();

export function kothApiEvidence(token, index) {
  if (typeof token !== 'string') return null;
  const tokenHash = createHash('sha256').update(token).digest('hex');
  const objectiveTenths = 5 + (index % 6);
  // Alternate the challenge-native objective budget while preserving the
  // ratio. The platform must normalize 5/10 and 5,000/10,000 identically.
  const firstObjective = Math.floor(index / 6) % 2 === 0
    ? { earned: objectiveTenths, possible: 10 }
    : { earned: objectiveTenths * 1_000, possible: 10_000 };
  return {
    tokenHash,
    activity: { earned: 1, possible: 1 },
    objectives: [
      firstObjective,
      { earned: 750, possible: 1_000 },
    ],
    isCrown: false,
  };
}

const KOTH_API_NORMALIZATION_SCALE = 1_000_000n;

function normalizedKothApiObjectiveTotal(team) {
  if (
    !team ||
    !team.activity ||
    !Number.isSafeInteger(team.activity.earned) ||
    !Number.isSafeInteger(team.activity.possible) ||
    team.activity.possible <= 0 ||
    team.activity.earned !== team.activity.possible ||
    !Array.isArray(team.objectives) ||
    team.objectives.length === 0
  ) {
    return null;
  }
  let total = 0n;
  for (const objective of team.objectives) {
    if (
      !objective ||
      !Number.isSafeInteger(objective.earned) ||
      !Number.isSafeInteger(objective.possible) ||
      objective.earned < 0 ||
      objective.possible <= 0 ||
      objective.earned > objective.possible
    ) {
      throw new TypeError('KotH API fixture objective must be a bounded integer ratio');
    }
    const earned = BigInt(objective.earned);
    const possible = BigInt(objective.possible);
    total += (earned * KOTH_API_NORMALIZATION_SCALE + possible / 2n) / possible;
  }
  return total > 0n ? total : null;
}

/** Apply the same unique-positive-normalized-leader Crown rule as RSCTF. */
export function assignUniqueKothApiCrown(teams) {
  if (!Array.isArray(teams)) throw new TypeError('KotH API teams must be an array');
  let best = null;
  let leaders = [];
  teams.forEach((team, index) => {
    team.isCrown = false;
    const score = normalizedKothApiObjectiveTotal(team);
    if (score == null) return;
    if (best == null || score > best) {
      best = score;
      leaders = [index];
    } else if (score === best) {
      leaders.push(index);
    }
  });
  if (leaders.length === 1) teams[leaders[0]].isCrown = true;
  return teams;
}

export function validateKothApiContext(contextModel) {
  const eligible = contextModel?.eligibleTokenHashes;
  const objectiveIds = contextModel?.objectiveIds;
  const objectiveSchemaHash = contextModel?.objectiveSchemaHash;
  if (
    contextModel?.apiVersion !== 'v2' ||
    typeof contextModel?.context !== 'string' ||
    !/^[0-9a-f]{64}$/.test(contextModel.context) ||
    !Number.isSafeInteger(contextModel?.cycleNumber) ||
    contextModel.cycleNumber < 1 ||
    !Number.isSafeInteger(contextModel?.resetAttempt) ||
    contextModel.resetAttempt < 0 ||
    !Number.isSafeInteger(contextModel?.roundNumber) ||
    contextModel.roundNumber < 1 ||
    !Number.isSafeInteger(contextModel?.cycleStartsAt) ||
    !Number.isSafeInteger(contextModel?.waveWindowStartsAt) ||
    contextModel.cycleStartsAt > contextModel.waveWindowStartsAt ||
    !Number.isSafeInteger(contextModel?.waveWindowEndsAt) ||
    contextModel.waveWindowEndsAt <= contextModel.waveWindowStartsAt ||
    !Number.isSafeInteger(contextModel?.cycleEndsAt) ||
    contextModel.cycleEndsAt <= contextModel.cycleStartsAt ||
    !Number.isSafeInteger(contextModel?.scoringEndsAt) ||
    contextModel.scoringEndsAt <= contextModel.cycleStartsAt ||
    contextModel.scoringEndsAt > contextModel.cycleEndsAt ||
    contextModel.scoringEndsAt + 1 < contextModel.waveWindowEndsAt ||
    !Number.isSafeInteger(contextModel?.generatedAt) ||
    !Array.isArray(eligible) ||
    eligible.some((hash) => typeof hash !== 'string' || !/^[0-9a-f]{64}$/.test(hash)) ||
    new Set(eligible).size !== eligible.length ||
    !Array.isArray(objectiveIds) ||
    objectiveIds.length > 16 ||
    objectiveIds.some((id) => typeof id !== 'string' || !/^[a-z][a-z0-9_.-]{0,63}$/.test(id)) ||
    new Set(objectiveIds).size !== objectiveIds.length ||
    (objectiveIds.length === 0
      ? objectiveSchemaHash !== null
      : typeof objectiveSchemaHash !== 'string' || !/^[0-9a-f]{64}$/.test(objectiveSchemaHash))
  ) {
    throw new Error('KotH API context response is malformed');
  }
  return contextModel;
}

/**
 * Report a simultaneous normalized arena snapshot through the trusted referee
 * API. Bearer capabilities are hashed locally and never enter the signed body.
 */
export async function kothApiObservation(
  gid,
  cid,
  secret,
  tokenOrTokens,
  { omitLast = false, deadlineMs } = {},
) {
  const gameId = Number(gid);
  const challengeId = Number(cid);
  const requestTimeout = () => {
    if (deadlineMs === undefined) return 30_000;
    const remainingMs = Math.floor(deadlineMs - performance.now());
    if (remainingMs <= 0) throw new Error('KotH API context retry deadline exhausted');
    return remainingMs;
  };
  const scope = `${gameId}:${challengeId}`;
  const cachedContext = kothContextValidators.get(scope);
  const contextResponse = await api(
    'GET',
    `/api/v1/koth/games/${gameId}/challenges/${challengeId}/context`,
    {
      ip: '10.9.9.10',
      timeoutMs: requestTimeout(),
      headers: {
        'x-rsctf-api-version': 'v2',
        ...(cachedContext?.etag ? { 'If-None-Match': cachedContext.etag } : {}),
      },
    },
  );
  let contextModel;
  if (contextResponse.status === 304 && cachedContext) {
    contextModel = cachedContext.model;
  } else if (contextResponse.status === 200) {
    contextModel = validateKothApiContext(unwrap(contextResponse));
    const etag = contextResponse.headers.get('etag');
    if (etag) kothContextValidators.set(scope, { etag, model: contextModel });
    else kothContextValidators.delete(scope);
  } else {
    const error = new Error(
      `fetch KotH API context → ${contextResponse.status} ${contextResponse.text?.slice(0, 200)}`,
    );
    error.status = contextResponse.status;
    error.code = contextResponse.json?.code;
    error.retryAfter = contextResponse.headers.get('retry-after');
    throw error;
  }
  const context = contextModel.context;
  const tokens = Array.isArray(tokenOrTokens)
    ? tokenOrTokens
    : typeof tokenOrTokens === 'string'
      ? [tokenOrTokens]
      : [];
  const selectedTokens =
    omitLast && tokens.length > 1 ? tokens.slice(0, -1) : tokens;
  const teams = selectedTokens
    .map((token, index) => kothApiEvidence(token, index))
    .filter(Boolean);
  assignUniqueKothApiCrown(teams);
  const previous = kothObservationLedgers.get(scope);
  // An acknowledgement can be lost after the snapshot and first objective
  // scheme commit. That commit changes the public context, but the durable
  // operation is still keyed by the prior exact body. Keep that pending intent
  // until the server returns its stored response or definitively rejects it.
  const sameSettlement = previous &&
    previous.cycleNumber === contextModel.cycleNumber &&
    previous.resetAttempt === contextModel.resetAttempt &&
    previous.roundNumber === contextModel.roundNumber;
  const replayingPriorSettlement = Boolean(previous?.pending && !sameSettlement);
  const ledger = previous?.pending
    ? previous
    : sameSettlement
    ? previous
    : {
        context,
        cycleNumber: contextModel.cycleNumber,
        resetAttempt: contextModel.resetAttempt,
        roundNumber: contextModel.roundNumber,
        sequence: 0,
        waves: [],
        pending: null,
      };
  if (ledger.waves.length >= 64) {
    throw new Error('KotH API load fixture exhausted the 64-wave settlement bound');
  }
  let pending = ledger.pending;
  if (!pending) {
    ledger.context = context;
    const earliest = ledger.waves.length === 0
      ? contextModel.waveWindowStartsAt
      : ledger.waves.at(-1).endedAtUnixMs;
    const endedAtUnixMs = Math.max(
      earliest,
      Math.min(Date.now(), contextModel.waveWindowEndsAt - 1),
    );
    if (endedAtUnixMs >= contextModel.waveWindowEndsAt) {
      throw new Error('KotH API load fixture settlement window has no timestamp left');
    }
    const nextWave = {
      waveId: `load-${contextModel.roundNumber}-${String(ledger.sequence).padStart(2, '0')}`,
      endedAtUnixMs,
      teams,
    };
    const proposedWaves = [...ledger.waves, nextWave];
    pending = {
      rawBody: JSON.stringify({
        context,
        objectiveIds: ['quality', 'throughput'],
        waves: proposedWaves,
      }),
      proposedWaves,
    };
    ledger.pending = pending;
    // Retain the exact intent before sending. A timeout can mean PostgreSQL
    // committed even though the acknowledgement never reached the referee.
    kothObservationLedgers.set(scope, ledger);
  }
  const { rawBody, proposedWaves } = pending;
  const timestamp = Math.max(Date.now(), lastKothObservationTimestamp + 1);
  lastKothObservationTimestamp = timestamp;
  const response = await api(
    'POST',
    `/api/v1/koth/games/${gameId}/challenges/${challengeId}/observations`,
    {
      rawBody,
      headers: kothObservationHeaders(secret, timestamp, gameId, challengeId, rawBody),
      ip: '10.9.9.10',
      timeoutMs: requestTimeout(),
    },
  );
  if (response.status === 200) {
    if (!replayingPriorSettlement) {
      ledger.context = context;
      ledger.cycleNumber = contextModel.cycleNumber;
      ledger.resetAttempt = contextModel.resetAttempt;
      ledger.roundNumber = contextModel.roundNumber;
      ledger.sequence += 1;
      ledger.waves = proposedWaves;
      ledger.pending = null;
      kothObservationLedgers.set(scope, ledger);
    } else {
      // The stored acknowledgement belongs to a prior settlement fence. Start
      // the newly fetched round cleanly on the next cycle instead of carrying
      // an append-only prefix across rounds.
      kothObservationLedgers.set(scope, {
        context,
        cycleNumber: contextModel.cycleNumber,
        resetAttempt: contextModel.resetAttempt,
        roundNumber: contextModel.roundNumber,
        sequence: 0,
        waves: [],
        pending: null,
      });
    }
  } else if ([400, 401, 403, 409, 413].includes(response.status)) {
    // A definitive rejection proves this exact intent did not commit. Retain
    // bodies across timeout, admission, and temporary server failures.
    ledger.pending = null;
    kothObservationLedgers.set(scope, ledger);
  }
  return response;
}

export function isRetriableKothApiContextFailure(value) {
  const status = Number(value?.status);
  const detail = value instanceof Error ? value.message : String(value?.text || '');
  const code = value instanceof Error ? value.code : value?.json?.code;
  if (status === 429 || status === 503) return true;
  if (
    value instanceof Error &&
    (value.name === 'AbortError' ||
      value.name === 'TimeoutError' ||
      /\b(?:fetch failed|connection reset|timed out|response lost)\b/i.test(detail))
  ) {
    return true;
  }
  return status === 409 && code === 'stale_context';
}

export function kothApiRetryDelayMs(value, attempt, random = Math.random) {
  if (!Number.isSafeInteger(attempt) || attempt < 0 || typeof random !== 'function') {
    throw new TypeError('KotH retry delay requires a non-negative attempt and random source');
  }
  const rawRetryAfter = value instanceof Error
    ? value.retryAfter
    : value?.headers?.get?.('retry-after');
  const retryAfterSeconds = Number(rawRetryAfter);
  const retryAfterMs = Number.isFinite(retryAfterSeconds) && retryAfterSeconds > 0
    ? Math.ceil(retryAfterSeconds * 1_000)
    : 0;
  const exponentialCap = Math.min(4_000, 250 * 2 ** Math.min(attempt, 8));
  const jitter = Math.floor(Math.max(0, Math.min(1, Number(random()))) * exponentialCap);
  return Math.max(retryAfterMs, jitter);
}

export async function kothApiCaptureWrite(
  gid,
  cid,
  secret,
  tokenOrTokens,
  options,
) {
  const deadlineMs = performance.now() + 10_000;
  let response;
  for (let attempt = 0; ; attempt += 1) {
    try {
      response = await kothApiObservation(
        gid,
        cid,
        secret,
        tokenOrTokens,
        { ...options, deadlineMs },
      );
      if (!isRetriableKothApiContextFailure(response)) break;
    } catch (error) {
      if (!isRetriableKothApiContextFailure(error)) throw error;
      response = error;
    }
    const remainingMs = deadlineMs - performance.now();
    if (remainingMs <= 0) {
      if (response instanceof Error) throw response;
      break;
    }
    await sleep(Math.min(kothApiRetryDelayMs(response, attempt), remainingMs));
  }
  if (response instanceof Error) throw response;
  if (response.status !== 200) {
    throw new Error(
      `write KotH Leaderboard evidence → ${response.status} ${response.text?.slice(0, 200)}`,
    );
  }
  return unwrap(response);
}

export function latestKothToken(gid, pid, cid) {
  return sql(
    `SELECT token.token FROM "KothTokens" token ` +
      `JOIN "KothCrownCycles" cycle ON cycle.id=token.cycle_id ` +
      `JOIN "Participations" participation ON participation.id=token.participation_id ` +
      `WHERE participation.game_id=${Number(gid)} AND token.participation_id=${Number(pid)} ` +
      `AND token.challenge_id=${Number(cid)} AND token.revoked_at IS NULL AND cycle.phase='Active' ` +
      `AND token.reset_attempt=cycle.reset_attempt ` +
      `ORDER BY cycle.cycle_number DESC, token.id DESC LIMIT 1`
  );
}
/** Snapshot every team's exact active-cycle KotH capability. */
export function kothCapturable(gid, cid) {
  const rows = sql(
    `SELECT DISTINCT ON (token.participation_id) token.participation_id||'|'||token.token ` +
      `FROM "KothTokens" token ` +
      `JOIN "Participations" participation ON participation.id=token.participation_id ` +
      `JOIN "KothCrownCycles" cycle ON cycle.id=token.cycle_id ` +
      `WHERE participation.game_id=${Number(gid)} AND token.challenge_id=${Number(cid)} ` +
      `AND token.revoked_at IS NULL AND cycle.phase='Active' ` +
      `AND token.reset_attempt=cycle.reset_attempt ` +
      `ORDER BY token.participation_id, cycle.cycle_number DESC, token.id DESC`
  );
  return (rows || '')
    .split('\n')
    .filter(keepId)
    .map((l) => {
      const [pid, token] = l.split('|');
      return { pid: Number(pid), token };
    });
}

/** Snapshot every team's event-scoped Leaderboard/API capability. */
export function kothApiCapturable(gid, cid) {
  const rows = sql(
    `SELECT capability.participation_id||'|'||capability.token ` +
      `FROM "KothApiTeamTokens" capability ` +
      `JOIN "Participations" participation ` +
      `ON participation.id=capability.participation_id ` +
      `AND participation.game_id=capability.game_id ` +
      `WHERE capability.game_id=${Number(gid)} ` +
      `AND capability.challenge_id=${Number(cid)} ` +
      `ORDER BY capability.participation_id`
  );
  return (rows || '')
    .split('\n')
    .filter(keepId)
    .map((line) => {
      const [pid, token] = line.split('|');
      return { pid: Number(pid), token };
    });
}

/** Durable KotH lifecycle state used by provision, play drivers, and integrity checks. */
export function crownReadiness(gid, cid) {
  const gameId = Number(gid);
  const challengeId = Number(cid);
  if (!Number.isSafeInteger(gameId) || gameId <= 0 || !Number.isSafeInteger(challengeId) || challengeId <= 0) {
    throw new Error(`invalid crown identity ${gid}/${cid}`);
  }
  const row = sql(
    `SELECT json_build_object(` +
      `'scoringStartRound',config.scoring_start_round,` +
      `'epochTicks',config.epoch_ticks,` +
      `'cycleTicks',config.cycle_ticks,` +
      `'confirmationTicks',config.claim_confirmation_ticks,` +
      `'latestRound',(SELECT COALESCE(max(number),0) FROM "AdRounds" round WHERE round.game_id=game.id),` +
      `'rosterCount',COALESCE(jsonb_array_length(config.roster_snapshot),0),` +
      `'cycleId',cycle.id,` +
      `'cycleNumber',cycle.cycle_number,` +
      `'phase',cycle.phase,` +
      `'resetAttempt',cycle.reset_attempt,` +
      `'containerId',target.container_id,` +
      `'replacementContainerId',cycle.replacement_container_id,` +
      `'oldContainerId',cycle.old_container_id,` +
      `'confirmationProgress',cycle.confirmation_progress,` +
      `'confirmedParticipationId',cycle.confirmed_participation_id,` +
      `'provisionalParticipationId',cycle.provisional_participation_id,` +
      `'tokenCount',(SELECT count(*) FROM "KothTokens" token WHERE token.cycle_id=cycle.id ` +
      `AND token.challenge_id=${challengeId} AND token.reset_attempt=cycle.reset_attempt ` +
      `AND token.revoked_at IS NULL),` +
      `'acquisitionCount',(SELECT count(*) FROM "KothAcquisitions" acquisition WHERE acquisition.cycle_id=cycle.id),` +
      `'cooldownCount',(SELECT count(*) FROM "KothCycleCooldowns" cooldown WHERE cooldown.cycle_id=cycle.id),` +
      `'receiptCount',(SELECT count(*) FROM "KothCycleAuditReceipts" receipt WHERE receipt.cycle_id=cycle.id)` +
      `)::text FROM "Games" game ` +
      `LEFT JOIN "KothOfficialConfigs" config ON config.game_id=game.id ` +
      `LEFT JOIN "KothTargets" target ON target.game_id=game.id AND target.challenge_id=${challengeId} ` +
      `LEFT JOIN LATERAL (SELECT crown.* FROM "KothCrownCycles" crown ` +
      `WHERE crown.game_id=game.id AND crown.challenge_id=${challengeId} ` +
      `ORDER BY crown.cycle_number DESC LIMIT 1) cycle ON TRUE ` +
      `WHERE game.id=${gameId}`
  );
  if (!row) throw new Error(`KotH lifecycle game ${gid} does not exist`);
  return JSON.parse(row);
}

export async function waitForCrownReady(
  gid,
  cid,
  expectedTeams,
  timeoutSeconds = Number(process.env.CROWN_READY_TIMEOUT_SECONDS || 360)
) {
  const wanted = validateReadinessArguments('crown', expectedTeams, timeoutSeconds);
  let snapshot = null;
  for (let waited = 0; waited <= timeoutSeconds; waited++) {
    snapshot = crownReadiness(gid, cid);
    const ready =
      snapshot.scoringStartRound > 0 &&
      snapshot.cycleId > 0 &&
      snapshot.cycleNumber > 0 &&
      snapshot.phase === 'Active' &&
      snapshot.rosterCount === wanted &&
      snapshot.tokenCount === wanted &&
      snapshot.containerId &&
      snapshot.containerId === snapshot.replacementContainerId;
    if (ready) return snapshot;
    if (waited < timeoutSeconds) await sleep(1000);
  }
  throw new Error(
    `KotH runtime lifecycle did not become ready within ${timeoutSeconds}s; ` +
      `wanted ${wanted} scoped capabilities, observed ${JSON.stringify(snapshot)}`
  );
}
export function teardownHill() {
  const existing = docker(['inspect', 'lckoth_hill']);
  if (existing.status !== 0) return;
  const removal = docker(['rm', '-f', 'lckoth_hill']);
  if (docker(['inspect', 'lckoth_hill']).status === 0) {
    throw new Error(`lifecycle hill teardown failed: ${removal.stderr.trim()}`);
  }
}

export { sleep };
