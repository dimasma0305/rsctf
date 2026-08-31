// Destructive, disposable-stack acceptance for the managed TargetReporter
// Leaderboard KotH path. This intentionally never submits with the legacy
// observer credential: configuring that resource selects Api scoring, while
// the challenge-owned runtime authenticates every capability and signs every
// dense observation with its injected, lifecycle-scoped reporter credential.
import { spawn, spawnSync } from 'node:child_process';
import { createHash, createHmac, randomUUID } from 'node:crypto';
import {
  chmodSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

import * as A from './applib.mjs';
import {
  managedKothAbusePlan,
  managedKothHarnessConfig,
  managedKothLoadPlan,
  managedKothOperationCycleId,
  managedKothSummaryMetric,
  validateManagedKothIntegrity,
  validateManagedKothRecovery,
  validateManagedReporterEnvironment,
  validateManagedReporterStatus,
} from './managed-koth-model.js';
import {
  BYOC_CONTAINER,
  docker,
  mintJwt,
  PG,
  RSCTF,
  retryTransientUntil,
  sleep,
  sql,
} from './lib.mjs';
import {
  acquireExclusiveProcessLock,
  loadOrchestrationLockPath,
} from './process-control.mjs';
import { stagedEventSchedule } from './provision-plan.js';

const ROSTER_SIZE = 2_000;
const ACTIVE_FLEET = 64;
const loadPlan = managedKothLoadPlan({ rosterSize: ROSTER_SIZE, activeFleet: ACTIVE_FLEET });
const abusePlan = managedKothAbusePlan({ admissionPerMinute: 3_000 });
const config = managedKothHarnessConfig(process.env);
const redisContainer = process.env.REDIS_CONTAINER || PG.replace(/-db-(\d+)$/, '-redis-$1');
const stackMarker = String(process.env.ADMIN_LIFECYCLE_STACK_MARKER || '').trim();
const tokenSandbox = mkdtempSync(join(tmpdir(), 'rsctf-managed-koth-'));
const tokenPath = join(tokenSandbox, 'capabilities.json');
const resourcePhases = [];
const gameIds = [];
let processLock;
let interrupted = false;

const onInterrupt = () => {
  interrupted = true;
};
process.on('SIGINT', onInterrupt);
process.on('SIGTERM', onInterrupt);

function requireCondition(condition, message) {
  if (!condition) throw new Error(message);
}

function throwIfInterrupted() {
  if (interrupted) throw new Error('managed KotH acceptance interrupted');
}

function unwrap(response) {
  return response?.json && Object.hasOwn(response.json, 'data') ? response.json.data : response?.json;
}

function inspectContainer(name, label = name) {
  const result = docker(['container', 'inspect', name]);
  requireCondition(result.status === 0, `cannot inspect disposable ${label}`);
  let records;
  try {
    records = JSON.parse(result.stdout);
  } catch {
    throw new Error(`cannot parse disposable ${label} inspection`);
  }
  requireCondition(Array.isArray(records) && records.length === 1, `${label} inspection is ambiguous`);
  return records[0];
}

function environmentMap(record) {
  return new Map((record?.Config?.Env || []).map((entry) => {
    const separator = String(entry).indexOf('=');
    return [String(entry).slice(0, separator), String(entry).slice(separator + 1)];
  }));
}

function assertDisposableStack() {
  requireCondition(
    /^[A-Za-z0-9][A-Za-z0-9._-]{7,127}$/.test(stackMarker),
    'ADMIN_LIFECYCLE_STACK_MARKER must identify the isolated disposable Compose stack',
  );
  const names = [...new Set([RSCTF, BYOC_CONTAINER, PG, redisContainer])];
  const records = names.map((name) => [name, inspectContainer(name)]);
  const projects = new Set();
  for (const [name, record] of records) {
    const env = environmentMap(record);
    requireCondition(
      env.get('RSCTF_ADMIN_LIFECYCLE_MARKER') === stackMarker,
      `${name} does not carry the exact disposable stack marker`,
    );
    const project = record?.Config?.Labels?.['com.docker.compose.project'];
    requireCondition(typeof project === 'string' && project.length > 0, `${name} is not Compose-owned`);
    projects.add(project);
  }
  requireCondition(projects.size === 1, 'managed KotH resources do not share one disposable Compose project');

  let reporterBaseUrl = null;
  for (const name of [...new Set([RSCTF, BYOC_CONTAINER])]) {
    const env = environmentMap(inspectContainer(name));
    requireCondition(
      env.get('RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE') === '3000',
      `${name} must run the isolated abuse profile with KotH capability IP admission 3000/minute`,
    );
    const candidate = String(env.get('RSCTF_KOTH_REPORTER_BASE_URL') || '').replace(/\/+$/, '');
    requireCondition(/^http:\/\/[A-Za-z0-9._-]+(?::\d+)?$/.test(candidate), `${name} has no private reporter callback origin`);
    reporterBaseUrl ??= candidate;
    requireCondition(candidate === reporterBaseUrl, 'server roles disagree on the reporter callback origin');
  }
  return reporterBaseUrl;
}

async function exactHealth(baseUrl, label) {
  const response = await A.api('GET', '/healthz', { baseUrl, timeoutMs: 5_000 });
  requireCondition(response.status === 200 && response.text === 'ok', `${label} healthz is not exact`);
}

async function waitUntil(label, operation, predicate, timeoutSeconds = 180) {
  let latest;
  let lastError;
  for (let waited = 0; waited <= timeoutSeconds; waited += 1) {
    throwIfInterrupted();
    try {
      latest = await operation();
      if (predicate(latest)) return latest;
      lastError = null;
    } catch (error) {
      lastError = error;
    }
    if (waited < timeoutSeconds) await sleep(1_000);
  }
  const suffix = lastError ? `: ${lastError.message}` : '';
  throw new Error(`${label} did not settle within ${timeoutSeconds}s${suffix}`);
}

function suffixPath(path, suffix) {
  const absolute = resolve(path);
  return absolute.endsWith('.json')
    ? `${absolute.slice(0, -5)}-${suffix}.json`
    : `${absolute}-${suffix}.json`;
}

function parseMemoryBytes(raw) {
  const match = String(raw).trim().match(/^([0-9.]+)(B|KiB|MiB|GiB)$/);
  if (!match) return null;
  const scale = { B: 1, KiB: 1024, MiB: 1024 ** 2, GiB: 1024 ** 3 }[match[2]];
  return Math.round(Number(match[1]) * scale);
}

function resourceSample(containers) {
  const unique = [...new Set(containers.filter(Boolean))];
  const result = spawnSync(
    'docker',
    ['stats', '--no-stream', '--format', '{{json .}}', ...unique],
    { encoding: 'utf8', timeout: 10_000 },
  );
  requireCondition(result.status === 0, 'docker resource sampling failed');
  const rows = String(result.stdout).trim().split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
  requireCondition(rows.length === unique.length, 'docker resource sampling omitted a declared container');
  return {
    atUnixMs: Date.now(),
    containers: rows.map((row) => {
      const cpuPercent = Number(String(row.CPUPerc || '').replace('%', ''));
      const memoryBytes = parseMemoryBytes(String(row.MemUsage || '').split('/')[0]);
      requireCondition(Number.isFinite(cpuPercent) && memoryBytes > 0, 'docker returned an invalid resource sample');
      return { name: row.Name, cpuPercent, memoryBytes };
    }),
  };
}

function assertK6Summary(path, phase) {
  let summary;
  try {
    summary = JSON.parse(readFileSync(path, 'utf8'));
  } catch {
    throw new Error(`managed KotH ${phase} summary is missing or malformed`);
  }
  requireCondition(managedKothSummaryMetric(summary, 'server_5xx', 'rate') === 0, `${phase} observed a 5xx`);
  requireCondition(managedKothSummaryMetric(summary, 'dropped_iterations', 'count') === 0, `${phase} dropped an arrival`);
  if (phase === 'abuse') {
    requireCondition(managedKothSummaryMetric(summary, 'invalid_capabilities_rejected', 'count') > 0, 'abuse did not reach 401');
    requireCondition(managedKothSummaryMetric(summary, 'invalid_capabilities_rate_limited', 'count') > 0, 'abuse did not reach 429');
    requireCondition(managedKothSummaryMetric(summary, 'invalid_retry_after', 'rate') === 0, 'abuse returned an invalid Retry-After');
  } else {
    requireCondition(
      managedKothSummaryMetric(summary, 'valid_capabilities_exercised', 'count') === ROSTER_SIZE,
      `${phase} did not authenticate the complete frozen roster`,
    );
    requireCondition(managedKothSummaryMetric(summary, 'valid_play_invalid', 'rate') === 0, `${phase} rejected a valid capability`);
  }
}

async function runK6Phase({ phase, arenaUrl, summaryPath, tokenFile, targetContainer }) {
  const plan = phase === 'abuse' ? abusePlan : loadPlan;
  if (phase === 'valid') await provisionPollingAdmin();
  const args = [
    'run',
    '--summary-export',
    summaryPath,
    new URL('./k6/managed-koth.js', import.meta.url).pathname,
  ];
  const environment = {
    PATH: process.env.PATH || '/usr/local/bin:/usr/bin:/bin',
    LANG: process.env.LANG || 'C.UTF-8',
    NO_PROXY: '*',
    TARGET: config.target,
    MANAGED_KOTH_ARENA: arenaUrl,
    MANAGED_KOTH_GAME: current.gameId,
    MANAGED_KOTH_CHALLENGE: current.challengeId,
    MANAGED_KOTH_ADMIN_TOKEN: current.pollerJwt,
    MANAGED_KOTH_TOKENS_FILE: tokenFile,
    MANAGED_KOTH_ACTIVE_FLEET: ACTIVE_FLEET,
    MANAGED_KOTH_PHASE: phase,
    MANAGED_KOTH_DURATION_SECONDS: plan.durationSeconds,
    RATE: plan.rate,
    VUS: plan.vus,
  };
  const child = spawn('k6', args, { stdio: ['ignore', 'inherit', 'pipe'], env: environment });
  let stderr = '';
  child.stderr.on('data', (chunk) => {
    process.stderr.write(chunk);
    stderr = `${stderr}${chunk}`.slice(-16_384);
  });
  const completion = new Promise((resolveCompletion) => {
    child.once('error', (error) => resolveCompletion({ error }));
    child.once('close', (code, signal) => resolveCompletion({ code, signal }));
  });
  let settled = false;
  completion.then(() => { settled = true; });
  const samples = [];
  try {
    while (!settled) {
      if (interrupted) child.kill('SIGTERM');
      samples.push(resourceSample([RSCTF, BYOC_CONTAINER, PG, targetContainer]));
      await Promise.race([completion, sleep(1_000)]);
    }
  } catch (error) {
    if (!settled) child.kill('SIGTERM');
    await completion;
    throw error;
  }
  const result = await completion;
  samples.push(resourceSample([RSCTF, BYOC_CONTAINER, PG, targetContainer]));
  resourcePhases.push({ phase, summaryPath, samples });
  if (result.error || result.code !== 0) {
    throw new Error(
      `managed KotH ${phase} k6 failed (${result.error?.message || result.signal || result.code}): ${stderr.trim().slice(-500)}`,
    );
  }
  requireCondition(samples.length >= Math.max(3, Math.floor(plan.durationSeconds / 3)), `${phase} resource series is incomplete`);
  assertK6Summary(summaryPath, phase);
}

const current = {
  gameId: null,
  challengeId: null,
  cohort: null,
  pollerJwt: null,
};

async function provisionPollingAdmin() {
  const tag = randomUUID().replaceAll('-', '');
  const email = `managed-koth-poller-${tag}@load.test`;
  const created = await A.api('POST', '/api/admin/users', {
    jwt: A.adminJwt(),
    body: [{
      userName: `mkothpoller${tag.slice(0, 16)}`,
      password: `Mkoth-${randomUUID()}!9`,
      email,
      realName: 'Managed KotH load poller',
    }],
  });
  requireCondition(created.status === 200, `polling identity creation returned ${created.status}`);

  const readIdentity = () => sql(
    `SELECT id::text||E'\\t'||security_stamp||E'\\t'||role::text ` +
      `FROM "AspNetUsers" WHERE normalized_email=upper('${email}')`,
  ).split('\t');
  const identity = readIdentity();
  requireCondition(
    identity.length === 3 && /^[0-9a-f-]{36}$/.test(identity[0]),
    'polling identity is incomplete',
  );
  const promoted = await A.api('PUT', `/api/admin/users/${identity[0]}`, {
    jwt: A.adminJwt(),
    body: { role: 'Admin' },
  });
  requireCondition(promoted.status === 200, `polling identity promotion returned ${promoted.status}`);
  const liveIdentity = readIdentity();
  requireCondition(
    liveIdentity.length === 3 && liveIdentity[0] === identity[0] && Number(liveIdentity[2]) === 3,
    'polling identity was not promoted to Admin',
  );
  current.pollerJwt = mintJwt(liveIdentity[0], liveIdentity[1], 3);
  requireCondition(current.pollerJwt !== A.adminJwt(), 'polling identity reused the bootstrap administrator');
}

function targetDatabaseSnapshot(gameId, challengeId) {
  const raw = sql(
    `SELECT concat_ws(E'\\t',cycle.id,cycle.cycle_number,cycle.reset_attempt,cycle.phase,` +
      `target.id,target.host,target.port,target.container_id,cycle.replacement_container_id,` +
      `reporter.routing_revision,(reporter.last_used_at IS NOT NULL)::text,reporter.hmac_secret) ` +
      `FROM "KothCrownCycles" cycle ` +
      `JOIN "KothTargets" target ON target.game_id=cycle.game_id AND target.challenge_id=cycle.challenge_id ` +
      `JOIN "KothTargetReporters" reporter ON reporter.cycle_id=cycle.id ` +
      `AND reporter.game_id=cycle.game_id AND reporter.challenge_id=cycle.challenge_id ` +
      `AND reporter.reset_attempt=cycle.reset_attempt ` +
      `WHERE cycle.game_id=${Number(gameId)} AND cycle.challenge_id=${Number(challengeId)} ` +
      `ORDER BY cycle.cycle_number DESC LIMIT 1`,
  );
  const fields = raw.split('\t');
  requireCondition(fields.length === 12, 'managed target database identity is incomplete');
  const [cycleId, cycleNumber, resetAttempt, phase, targetId, host, port,
    containerId, replacementContainerId, routingRevision, reporterUsed, secret] = fields;
  requireCondition(
    phase === 'Active' &&
      /^[a-f0-9]{64}$/.test(containerId) &&
      containerId === replacementContainerId &&
      /^koth_target_[A-Za-z0-9_-]{32,128}$/.test(secret),
    'managed target database identity is inconsistent',
  );
  return {
    cycleId: Number(cycleId),
    cycleNumber: Number(cycleNumber),
    resetAttempt: Number(resetAttempt),
    targetId: Number(targetId),
    host,
    port: Number(port),
    containerId,
    routingRevision,
    credentialRevision: createHash('sha256').update(secret).digest('hex').slice(0, 32),
    reporterUsed: reporterUsed === 'true',
    secret,
  };
}

function inspectManagedTarget(snapshot, reporterBaseUrl) {
  const record = inspectContainer(snapshot.containerId, 'managed target');
  requireCondition(record?.State?.Running === true, 'managed target is not running');
  const injected = validateManagedReporterEnvironment(record?.Config?.Env, {
    gameId: current.gameId,
    challengeId: current.challengeId,
    platformUrl: reporterBaseUrl,
  });
  requireCondition(
    injected.RSCTF_KOTH_REPORTER_SECRET === snapshot.secret,
    'managed target and lifecycle reporter credentials disagree',
  );
  const operation = record?.Config?.Labels?.['rsctf.operation'];
  const expectedOperation =
    `koth-cycle:${snapshot.cycleId}:attempt:${snapshot.resetAttempt}:managed-reporter-v2:` +
    `${snapshot.routingRevision}:${snapshot.credentialRevision}`;
  requireCondition(operation === expectedOperation, 'managed target operation identity is not credential-fenced');
  requireCondition(managedKothOperationCycleId(operation) === snapshot.cycleId, 'managed target operation scope is invalid');
  return {
    ...snapshot,
    operation,
    arenaUrl: `http://${snapshot.host.includes(':') ? `[${snapshot.host}]` : snapshot.host}:${snapshot.port}`,
  };
}

async function assertReporterFreeBootstrapTarget() {
  const fields = sql(
    `SELECT host||E'\\t'||port::text||E'\\t'||container_id ` +
      `FROM "KothTargets" WHERE game_id=${current.gameId} AND challenge_id=${current.challengeId}`,
  ).split('\t');
  requireCondition(
    fields.length === 3 && Number(fields[1]) > 0 && /^[a-f0-9]{64}$/.test(fields[2]),
    'pre-cycle target identity is incomplete',
  );
  const [host, port, containerId] = fields;
  const record = inspectContainer(containerId, 'pre-cycle managed target');
  requireCondition(
    !(record?.Config?.Env || []).some((entry) => String(entry).startsWith('RSCTF_KOTH_')),
    'pre-cycle target received a lifecycle reporter credential',
  );
  const arenaUrl = `http://${host.includes(':') ? `[${host}]` : host}:${Number(port)}`;
  await waitUntil(
    'pre-cycle target health without reporter configuration',
    async () => {
      await exactHealth(arenaUrl, 'pre-cycle managed target');
      return A.api('GET', '/reporter-status', { baseUrl: arenaUrl, timeoutMs: 5_000 });
    },
    (status) =>
      status.status === 200 &&
      status.json?.reporterConfigured === false &&
      status.json?.reporterHealthy === true &&
      status.json?.contextRefreshes === 0 &&
      status.json?.eligibleRoster === 0,
    60,
  );
}

async function reporterStatus(target) {
  const response = await A.api('GET', '/reporter-status', {
    baseUrl: target.arenaUrl,
    timeoutMs: 5_000,
  });
  requireCondition(response.status === 200, 'managed target reporter status is unavailable');
  return validateManagedReporterStatus(response.json);
}

async function managedContext(expectedRoster) {
  const response = await A.api(
    'GET',
    `/api/v1/koth/games/${current.gameId}/challenges/${current.challengeId}/context`,
    { headers: { 'x-rsctf-api-version': 'v2' }, timeoutMs: 10_000 },
  );
  requireCondition(response.status === 200, `managed context returned ${response.status}`);
  const context = A.validateKothApiContext(unwrap(response));
  const vary = new Set(String(response.headers.get('vary') || '').split(',').map((item) => item.trim().toLowerCase()));
  requireCondition(response.headers.get('cache-control') === 'no-store', 'managed context is cacheable');
  requireCondition(vary.has('x-rsctf-api-version'), 'managed context omits its API-version variance');
  requireCondition(context.eligibleTokenHashes.length === expectedRoster, `managed context roster is ${context.eligibleTokenHashes.length}/${expectedRoster}`);
  return context;
}

function sortedCapabilities() {
  const rows = A.kothApiCapturable(current.gameId, current.challengeId).map(({ pid, token }) => ({
    pid,
    token,
    tokenHash: createHash('sha256').update(token).digest('hex'),
  })).sort((left, right) => left.tokenHash.localeCompare(right.tokenHash));
  requireCondition(rows.length === ROSTER_SIZE, `managed capability roster is ${rows.length}/${ROSTER_SIZE}`);
  requireCondition(new Set(rows.map(({ pid }) => pid)).size === ROSTER_SIZE, 'managed capability roster repeats a participation');
  requireCondition(new Set(rows.map(({ token }) => token)).size === ROSTER_SIZE, 'managed capability roster repeats a token');
  return rows;
}

function writeCapabilities(rows) {
  writeFileSync(tokenPath, JSON.stringify(rows.map(({ token }) => token)), { mode: 0o600 });
  chmodSync(tokenPath, 0o600);
}

async function arenaPlay(target, token, score = 0) {
  return A.api('POST', '/play', {
    baseUrl: target.arenaUrl,
    body: { token, score },
    timeoutMs: 10_000,
  });
}

async function assertHiddenEvent(ordinaryJwt) {
  const paths = [
    `/api/game/${current.gameId}`,
    `/api/game/${current.gameId}/scoreboard`,
    `/api/game/${current.gameId}/ad/koth/scoreboard`,
  ];
  for (const path of paths) {
    const anonymous = await A.api('GET', path);
    const ordinary = await A.api('GET', path, { jwt: ordinaryJwt });
    const admin = await A.api('GET', path, { jwt: A.adminJwt() });
    requireCondition(anonymous.status === 404 && ordinary.status === 404, `${path} exposed the hidden event`);
    requireCondition(admin.status === 200, `${path} is unavailable to the administrator`);
  }
  const operator = await A.api('GET', `/api/edit/games/${current.gameId}/ad/koth/state`, { jwt: A.adminJwt() });
  requireCondition(operator.status === 200, 'hidden KotH operator state is unavailable to the administrator');
}

function capabilityState(participationId) {
  const row = sql(
    `SELECT token||E'\\t'||generation::text||E'\\t'||revocation_pending::text ` +
      `FROM "KothApiTeamTokens" WHERE game_id=${current.gameId} ` +
      `AND challenge_id=${current.challengeId} AND participation_id=${Number(participationId)}`,
  ).split('\t');
  requireCondition(row.length === 3 && /^koth_/.test(row[0]), 'managed capability state is incomplete');
  return { token: row[0], generation: Number(row[1]), revocationPending: row[2] === 'true' };
}

async function updateParticipation(participationId, status) {
  const response = await A.api('PUT', `/api/admin/participation/${Number(participationId)}`, {
    jwt: A.adminJwt(),
    body: { status },
    timeoutMs: 120_000,
  });
  requireCondition(response.status >= 200 && response.status < 300, `${status} participation returned ${response.status}`);
}

async function exerciseRevocation(target, capability) {
  await A.setAdScoringPaused(current.gameId, true);
  const before = capabilityState(capability.pid);
  await updateParticipation(capability.pid, 'Suspended');
  const suspended = capabilityState(capability.pid);
  requireCondition(
    suspended.generation === before.generation || suspended.generation === before.generation + 1,
    'capability suspension advanced more than one generation',
  );
  const suspendedContext = await managedContext(ROSTER_SIZE - 1);
  requireCondition(!suspendedContext.eligibleTokenHashes.includes(capability.tokenHash), 'suspended capability remained eligible');
  const rejected = await arenaPlay(target, before.token);
  requireCondition(rejected.status === 401, `suspended capability returned ${rejected.status}, expected 401`);

  await updateParticipation(capability.pid, 'Accepted');
  const rotated = await waitUntil(
    'capability reinstatement rotation',
    async () => capabilityState(capability.pid),
    (state) => !state.revocationPending && state.generation === before.generation + 1 && state.token !== before.token,
  );
  const stale = await arenaPlay(target, before.token);
  requireCondition(stale.status === 401, `rotated capability returned ${stale.status}, expected 401`);
  const accepted = await arenaPlay(target, rotated.token, 0);
  const rotatedHash = createHash('sha256').update(rotated.token).digest('hex');
  requireCondition(
    accepted.status === 200 && accepted.json?.accepted === true && accepted.json?.teamId === rotatedHash,
    'rotated capability did not authenticate through the managed challenge',
  );
  const restoredContext = await managedContext(ROSTER_SIZE);
  requireCondition(
    restoredContext.eligibleTokenHashes.includes(rotatedHash) &&
      !restoredContext.eligibleTokenHashes.includes(capability.tokenHash),
    'reinstated roster context did not replace the old capability identity',
  );
  return { before, rotated };
}

async function signedOldReporterProbe(secret) {
  const rawBody = '{}';
  const timestamp = String(Date.now());
  const message = `${timestamp}.${current.gameId}.${current.challengeId}.${rawBody}`;
  const signature = createHmac('sha256', secret).update(message).digest('hex');
  const response = await A.api(
    'POST',
    `/api/v1/koth/games/${current.gameId}/challenges/${current.challengeId}/observations`,
    {
      rawBody,
      headers: {
        'x-rsctf-timestamp': timestamp,
        'x-rsctf-signature': `sha256=${signature}`,
      },
      timeoutMs: 10_000,
    },
  );
  requireCondition(response.status === 401, `revoked target reporter credential returned ${response.status}`);
}

async function recoverManagedTarget(target, reporterBaseUrl) {
  requireCondition(
    A.adScoringPaused(current.gameId),
    'managed target recovery requires scoring to remain paused',
  );
  const marker = `managed-koth-recovery-${randomUUID()}`;
  const changed = sql(
    `UPDATE "KothCrownCycles" cycle SET phase='ReadinessPending',` +
      `readiness_error='${marker}',last_error='${marker}',updated_at=clock_timestamp() ` +
      `FROM "KothTargets" target,"Games" game,"GameChallenges" challenge ` +
      `WHERE cycle.id=${target.cycleId} AND cycle.game_id=${current.gameId} ` +
      `AND cycle.challenge_id=${current.challengeId} AND cycle.phase='Active' ` +
      `AND cycle.reset_attempt=${target.resetAttempt} ` +
      `AND cycle.replacement_container_id='${target.containerId}' ` +
      `AND target.game_id=cycle.game_id AND target.challenge_id=cycle.challenge_id ` +
      `AND target.container_id=cycle.replacement_container_id ` +
      `AND game.id=cycle.game_id AND game.hidden=TRUE ` +
      `AND challenge.game_id=cycle.game_id AND challenge.id=cycle.challenge_id ` +
      `RETURNING cycle.id`,
  );
  requireCondition(Number(changed) === target.cycleId, 'managed recovery fault did not bind the exact active cycle');
  const stopped = docker(['stop', '--time', '2', target.containerId]);
  requireCondition(stopped.status === 0, 'managed target could not be stopped for recovery');
  const response = await retryTransientUntil(
    ({ timeoutMs }) => A.api(
      'POST',
      `/api/edit/games/${current.gameId}/ad/koth/${current.challengeId}/recover`,
      { jwt: A.adminJwt(), timeoutMs },
    ),
    (candidate) => candidate?.status === 409 && [
      'replacement container is still transitioning',
      'checker exit 2',
      'checker timed out',
    ].includes(candidate.json?.title),
    { budgetMs: 30_000, delayMs: 500 },
  );
  requireCondition(
    response.status === 200 && unwrap(response)?.resetPhase === 'Active',
    `managed target recovery did not converge: HTTP ${response.status} ${response.json?.title || ''}`.trim(),
  );
  const recovered = await waitUntil(
    'managed target replacement',
    async () => inspectManagedTarget(targetDatabaseSnapshot(current.gameId, current.challengeId), reporterBaseUrl),
    (candidate) => candidate.containerId !== target.containerId && candidate.resetAttempt === target.resetAttempt + 1,
    240,
  );
  validateManagedKothRecovery(
    {
      cycleId: target.cycleId,
      resetAttempt: target.resetAttempt,
      containerId: target.containerId,
      credentialRevision: target.credentialRevision,
    },
    {
      cycleId: recovered.cycleId,
      resetAttempt: recovered.resetAttempt,
      containerId: recovered.containerId,
      credentialRevision: recovered.credentialRevision,
      operation: recovered.operation,
    },
  );
  requireCondition(docker(['container', 'inspect', target.containerId]).status !== 0, 'recovery retained the stopped target');
  await signedOldReporterProbe(target.secret);
  return recovered;
}

function currentSnapshotIdentity() {
  const fields = sql(
    `SELECT snapshot.ad_round_id::text||E'\\t'||encode(snapshot.snapshot_hash,'hex')||E'\\t'||` +
      `(SELECT count(*) FROM "KothApiSnapshotWaves" wave WHERE wave.target_id=snapshot.target_id)::text||E'\\t'||` +
      `(SELECT count(*) FROM "KothApiSnapshotScores" score WHERE score.target_id=snapshot.target_id)::text ` +
      `FROM "KothApiSnapshots" snapshot WHERE snapshot.game_id=${current.gameId} ` +
      `AND snapshot.challenge_id=${current.challengeId}`,
  ).split('\t');
  requireCondition(
    fields.length === 4 && Number(fields[0]) > 0 && /^[a-f0-9]{64}$/.test(fields[1]),
    'managed current snapshot identity is incomplete',
  );
  const evidence = sql(
    `SELECT coalesce(jsonb_agg(jsonb_build_array(` +
      `wave.wave_id,(extract(epoch FROM wave.ended_at)*1000)::bigint,` +
      `score.participation_id,score.activity_earned,score.activity_possible,` +
      `score.objective_earned,score.objective_possible,score.objective_count,score.is_crown` +
    `) ORDER BY wave.ended_at,wave.wave_id,score.participation_id),'[]'::jsonb)::text ` +
    `FROM "KothApiSnapshots" snapshot ` +
    `JOIN "KothApiSnapshotWaves" wave ON wave.target_id=snapshot.target_id ` +
    `JOIN "KothApiSnapshotScores" score ON score.target_id=wave.target_id AND score.wave_id=wave.wave_id ` +
    `WHERE snapshot.game_id=${current.gameId} AND snapshot.challenge_id=${current.challengeId}`,
  );
  requireCondition(evidence.startsWith('[['), 'managed current snapshot evidence is incomplete');
  return {
    roundId: Number(fields[0]),
    snapshotHash: fields[1],
    evidenceHash: createHash('sha256').update(evidence).digest('hex'),
    waves: Number(fields[2]),
    rows: Number(fields[3]),
  };
}

async function restartManagedReporterProcess(target, reporterBaseUrl) {
  const before = currentSnapshotIdentity();
  requireCondition(before.waves === 1 && before.rows === ROSTER_SIZE, 'pre-restart snapshot is not one exact dense wave');
  const restarted = docker(['restart', '--time', '2', target.containerId]);
  requireCondition(restarted.status === 0, 'managed reporter process restart failed');
  const sameTarget = await waitUntil(
    'same-generation managed reporter restart',
    async () => {
      const candidate = inspectManagedTarget(
        targetDatabaseSnapshot(current.gameId, current.challengeId),
        reporterBaseUrl,
      );
      await exactHealth(candidate.arenaUrl, 'restarted managed target');
      return candidate;
    },
    (candidate) => candidate.containerId === target.containerId &&
      candidate.resetAttempt === target.resetAttempt &&
      candidate.credentialRevision === target.credentialRevision,
    120,
  );
  return { target: sameTarget, before };
}

function integritySnapshot() {
  const raw = sql(
    `WITH ranked AS (` +
      `SELECT result.*,max(result.objective_rate) OVER (PARTITION BY result.ad_round_id) AS best_objective ` +
      `FROM "KothApiScoreResults" result WHERE result.game_id=${current.gameId} ` +
      `AND result.challenge_id=${current.challengeId}` +
    `), scored AS (` +
      `SELECT result.ad_round_id,count(*)::bigint AS rows,` +
      `count(*) FILTER (WHERE result.core_rate=0.0)::bigint AS zero_rows,` +
      `count(*) FILTER (WHERE result.core_rate>0.0)::bigint AS positive_rows,` +
      `count(*) FILTER (WHERE result.lead_credit=1.0)::bigint AS crown_rows,` +
      `count(*) FILTER (WHERE result.objective_rate=result.best_objective AND result.best_objective>0.0)::bigint AS best_rows,` +
      `count(*) FILTER (WHERE (result.lead_credit=1.0) <> ` +
        `(result.objective_rate=result.best_objective AND result.best_objective>0.0))::bigint AS crown_mismatches,` +
      `min(result.activity_possible)::bigint AS min_activity_possible,` +
      `max(result.activity_possible)::bigint AS max_activity_possible,` +
      `count(*) FILTER (WHERE result.activity_earned<>result.activity_possible ` +
        `OR result.objective_count<>1 OR result.objective_possible<>1000000 ` +
        `OR result.objective_earned<0 OR result.objective_earned>result.objective_possible ` +
        `OR result.lead_credit NOT IN (0.0,1.0))::bigint AS invalid_rows ` +
      `FROM ranked result GROUP BY result.ad_round_id` +
    `), current_target AS (` +
      `SELECT target.id,cycle.reset_attempt,(reporter.last_used_at IS NOT NULL) AS reporter_used ` +
      `FROM "KothTargets" target JOIN "KothCrownCycles" cycle ` +
      `ON cycle.game_id=target.game_id AND cycle.challenge_id=target.challenge_id ` +
      `AND cycle.replacement_container_id=target.container_id AND cycle.phase='Active' ` +
      `JOIN "KothTargetReporters" reporter ON reporter.cycle_id=cycle.id ` +
      `AND reporter.reset_attempt=cycle.reset_attempt ` +
      `WHERE target.game_id=${current.gameId} AND target.challenge_id=${current.challengeId}` +
    `) SELECT json_build_object(` +
      `'rosterCount',(SELECT count(*) FROM "Participations" WHERE game_id=${current.gameId} AND status=1),` +
      `'capabilityCount',(SELECT count(*) FROM "KothApiTeamTokens" WHERE game_id=${current.gameId} AND challenge_id=${current.challengeId}),` +
      `'pendingRevocations',(SELECT count(*) FROM "KothApiTeamTokens" WHERE game_id=${current.gameId} AND challenge_id=${current.challengeId} AND revocation_pending),` +
      `'scorableRounds',(SELECT count(*) FROM scored),` +
      `'denseRows',COALESCE((SELECT sum(rows) FROM scored),0),` +
      `'zeroRows',COALESCE((SELECT sum(zero_rows) FROM scored),0),` +
      `'positiveRows',COALESCE((SELECT sum(positive_rows) FROM scored),0),` +
      `'crownRows',COALESCE((SELECT sum(crown_rows) FROM scored),0),` +
      `'uniqueCrownRounds',(SELECT count(*) FROM scored WHERE crown_rows=1 AND best_rows=1 AND crown_mismatches=0),` +
      `'crownMismatches',COALESCE((SELECT sum(crown_mismatches) FROM scored),0),` +
      `'denseRounds',(SELECT count(*) FROM scored WHERE rows=${ROSTER_SIZE}),` +
      `'fullRosterWaves',COALESCE((SELECT sum(min_activity_possible/1000000) FROM scored WHERE min_activity_possible=max_activity_possible),0),` +
      `'invalidRows',COALESCE((SELECT sum(invalid_rows) FROM scored),0),` +
      `'exclusiveRows',(` +
        // Leaderboard writes snapshot currency to marker_observed, so only
        // participant ownership fields distinguish the exclusive modes here.
        `(SELECT count(*) FROM "KothControlResults" result WHERE result.game_id=${current.gameId} ` +
          `AND result.challenge_id=${current.challengeId} AND (` +
          `result.controlling_participation_id IS NOT NULL OR result.responsible_participation_id IS NOT NULL ` +
          `OR result.token_id IS NOT NULL OR result.provisional_participation_id IS NOT NULL ` +
          `OR result.confirmed_participation_id IS NOT NULL OR result.confirmation_streak<>0)) + ` +
        `(SELECT count(*) FROM "KothTargets" WHERE game_id=${current.gameId} AND challenge_id=${current.challengeId} AND holder_participation_id IS NOT NULL) + ` +
        `(SELECT count(*) FROM "KothAcquisitions" acquisition JOIN "KothCrownCycles" cycle ON cycle.id=acquisition.cycle_id WHERE cycle.game_id=${current.gameId} AND cycle.challenge_id=${current.challengeId}) + ` +
        `(SELECT count(*) FROM "KothCycleCooldowns" cooldown JOIN "KothCrownCycles" cycle ON cycle.id=cooldown.cycle_id WHERE cycle.game_id=${current.gameId} AND cycle.challenge_id=${current.challengeId})` +
      `),` +
      `'duplicateRows',(SELECT count(*) FROM (` +
        `SELECT 1 FROM "KothApiScoreResults" WHERE game_id=${current.gameId} AND challenge_id=${current.challengeId} ` +
        `GROUP BY ad_round_id,participation_id HAVING count(*)<>1) duplicate),` +
      `'snapshotRows',(SELECT count(*) FROM "KothApiSnapshotScores" score JOIN current_target target ON target.id=score.target_id),` +
      `'snapshotWaves',(SELECT count(*) FROM "KothApiSnapshotWaves" wave JOIN current_target target ON target.id=wave.target_id),` +
      `'reporterResetAttempt',(SELECT reset_attempt FROM current_target),` +
      `'reporterUsed',(SELECT reporter_used FROM current_target)` +
    `)::text`,
  );
  requireCondition(raw, 'managed KotH integrity query returned no row');
  return JSON.parse(raw);
}

async function provision(reporterBaseUrl) {
  const now = A.nowMs();
  const schedule = stagedEventSchedule(now, 30 * 60 * 1_000);
  current.gameId = await A.createGame({
    title: `LOADTEST-MANAGED-KOTH-${now}`,
    hidden: true,
    practiceMode: false,
    acceptWithoutReview: true,
    start: schedule.stagingStart,
    end: schedule.stagingEnd,
    teamMemberCountLimit: 1,
    containerCountLimit: 1,
    allowUserSubmissions: false,
    adWarmupSeconds: 1,
    adTickSeconds: 30,
    adFlagLifetimeTicks: 5,
    adGetflagWindowFraction: 0.9,
    adMinGracePeriodSeconds: 1,
    adResetCooldownMinutes: 5,
    adEpochTicks: 2,
    kothEpochTicks: 2,
    kothCycleTicks: 1,
    kothChampionCooldownTicks: 0,
    kothClaimConfirmationTicks: 1,
  });
  gameIds.push(current.gameId);
  await A.setAdScoringPaused(current.gameId, true);
  sql(
    `UPDATE "Games" SET ad_warmup_seconds=1,ad_tick_seconds=30,ad_flag_lifetime_ticks=5,` +
      `ad_getflag_window_fraction=0.9,ad_min_grace_period_seconds=1,` +
      `koth_epoch_ticks=2,koth_cycle_ticks=1,koth_champion_cooldown_ticks=0,` +
      `koth_claim_confirmation_ticks=1 WHERE id=${current.gameId} AND hidden=TRUE`,
  );
  current.challengeId = await A.createChallenge(current.gameId, {
    title: 'managed-target-reporter',
    category: 'Pwn',
    type: 'KingOfTheHill',
  });
  const image = A.buildManagedKothImage();
  const checker = A.prepareKothChecker(current.gameId, current.challengeId);
  await A.setChallenge(current.gameId, current.challengeId, {
    content: 'Disposable managed TargetReporter acceptance fixture.',
    containerImage: image,
    memoryLimit: 128,
    cpuCount: 1,
    exposePort: 8080,
    adAllowEgress: false,
    adCheckerImage: checker,
  });
  await A.rebuildChallengeImage(current.gameId, current.challengeId, image, 'managed KotH target');
  await A.addFlags(current.gameId, current.challengeId, ['flag{managed_koth_load_placeholder}']);
  await A.configureKothApiObserver(current.gameId, current.challengeId);
  await A.setChallenge(current.gameId, current.challengeId, { isEnabled: true });
  current.cohort = A.seedCohort(current.gameId, ROSTER_SIZE);
  const initial = await A.api('POST', `/api/edit/games/${current.gameId}/ad/EnsureContainers`, {
    jwt: A.adminJwt(),
    headers: { 'idempotency-key': randomUUID() },
    timeoutMs: 180_000,
  });
  requireCondition(initial.status === 200, `initial managed target provisioning returned ${initial.status}`);
  await assertReporterFreeBootstrapTarget();
  requireCondition(
    sql(`SELECT hidden::text||'|'||ad_scoring_paused::text FROM "Games" WHERE id=${current.gameId}`) === 'true|true',
    'managed event was not hidden and paused throughout provisioning',
  );
  await A.setGameSchedule(current.gameId, schedule.liveStart, schedule.liveEnd);
  await A.setAdScoringPaused(current.gameId, false);
  await A.waitForCrownReady(current.gameId, current.challengeId, ROSTER_SIZE, 360);
  const target = await waitUntil(
    'managed TargetReporter lifecycle',
    async () => inspectManagedTarget(targetDatabaseSnapshot(current.gameId, current.challengeId), reporterBaseUrl),
    (candidate) => candidate.containerId && candidate.resetAttempt >= 0,
    360,
  );
  await exactHealth(target.arenaUrl, 'managed target');
  await waitUntil(
    'managed reporter bootstrap',
    () => reporterStatus(target),
    (status) => status.reporterConfigured && status.reporterHealthy &&
      status.contextRefreshes > 0 && status.eligibleRoster === ROSTER_SIZE,
  );
  return target;
}

async function main() {
  const reporterBaseUrl = assertDisposableStack();
  processLock = await acquireExclusiveProcessLock(loadOrchestrationLockPath, {
    label: 'managed KotH acceptance',
    metadata: { target: config.target },
  });
  await exactHealth(config.target, 'platform');
  await A.preflight();

  let target = await provision(reporterBaseUrl);
  const ordinaryJwt = mintJwt(current.cohort.userIds[0], undefined, 1);
  await assertHiddenEvent(ordinaryJwt);
  const initialContext = await managedContext(ROSTER_SIZE);
  requireCondition(
    initialContext.objectiveIds.length === 0 && initialContext.objectiveSchemaHash == null,
    'managed objective schema froze before the challenge reported evidence',
  );

  let capabilities = sortedCapabilities();
  writeCapabilities(capabilities);
  await runK6Phase({
    phase: 'valid',
    arenaUrl: target.arenaUrl,
    summaryPath: resolve(config.summaryPath),
    tokenFile: tokenPath,
    targetContainer: target.containerId,
  });
  const firstStatus = await waitUntil(
    'two separately finalized managed waves',
    () => reporterStatus(target),
    (status) => {
      validateManagedReporterStatus(status, { ...loadPlan, minimumReports: 2 });
      return true;
    },
    180,
  );
  requireCondition(firstStatus.submittedWaves === firstStatus.successfulReports, 'managed reporter batched multiple waves into one body');
  const frozenContext = await managedContext(ROSTER_SIZE);
  requireCondition(
    frozenContext.context !== initialContext.context &&
      JSON.stringify(frozenContext.objectiveIds) === JSON.stringify(['official-score']) &&
      /^[0-9a-f]{64}$/.test(frozenContext.objectiveSchemaHash || ''),
    'managed reporter did not refetch the schema-frozen context',
  );
  await waitUntil(
    'pre-recovery dense score rows',
    async () => integritySnapshot(),
    (evidence) => evidence.scorableRounds >= 2 && evidence.denseRows === evidence.scorableRounds * ROSTER_SIZE,
    180,
  );

  const restart = await restartManagedReporterProcess(target, reporterBaseUrl);
  target = restart.target;
  await waitUntil(
    'restarted reporter active context',
    () => reporterStatus(target),
    (status) => status.reporterConfigured && status.reporterHealthy &&
      status.contextRefreshes > 0 && status.eligibleRoster === ROSTER_SIZE,
    120,
  );
  await runK6Phase({
    phase: 'valid',
    arenaUrl: target.arenaUrl,
    summaryPath: suffixPath(config.summaryPath, 'prefix'),
    tokenFile: tokenPath,
    targetContainer: target.containerId,
  });
  await waitUntil(
    'restarted reporter exact dense wave',
    () => reporterStatus(target),
    (status) => {
      validateManagedReporterStatus(status, { ...loadPlan, minimumReports: 1 });
      return true;
    },
    120,
  );
  const reconstructed = currentSnapshotIdentity();
  const sameRoundPrefix = reconstructed.roundId === restart.before.roundId &&
    reconstructed.evidenceHash === restart.before.evidenceHash;
  requireCondition(
    (sameRoundPrefix || reconstructed.roundId > restart.before.roundId) &&
      reconstructed.waves === 1 && reconstructed.rows === ROSTER_SIZE,
    `restarted reporter did not submit a monotonic exact dense wave: ${JSON.stringify({
      before: restart.before,
      reconstructed,
    })}`,
  );

  const revoked = capabilities[Math.floor(capabilities.length / 2)];
  await exerciseRevocation(target, revoked);
  const resetContext = await managedContext(ROSTER_SIZE);
  target = await recoverManagedTarget(target, reporterBaseUrl);
  await exactHealth(target.arenaUrl, 'recovered managed target');
  await waitUntil(
    'recovered reporter bootstrap',
    () => reporterStatus(target),
    (status) => status.reporterConfigured && status.reporterHealthy &&
      status.contextRefreshes > 0 && status.eligibleRoster === ROSTER_SIZE,
  );
  const recoveredContext = await managedContext(ROSTER_SIZE);
  requireCondition(recoveredContext.context !== resetContext.context, 'target reset did not fence the reporter context');

  capabilities = sortedCapabilities();
  writeCapabilities(capabilities);
  await A.setAdScoringPaused(current.gameId, false);
  await runK6Phase({
    phase: 'valid',
    arenaUrl: target.arenaUrl,
    summaryPath: suffixPath(config.summaryPath, 'restart'),
    tokenFile: tokenPath,
    targetContainer: target.containerId,
  });
  const recoveredStatus = await waitUntil(
    'recovered reporter waves',
    () => reporterStatus(target),
    (status) => {
      validateManagedReporterStatus(status, { ...loadPlan, minimumReports: 2 });
      return true;
    },
    180,
  );

  await runK6Phase({
    phase: 'abuse',
    arenaUrl: target.arenaUrl,
    summaryPath: suffixPath(config.summaryPath, 'abuse'),
    tokenFile: tokenPath,
    targetContainer: target.containerId,
  });
  await waitUntil(
    'reporter isolation after capability abuse',
    () => reporterStatus(target),
    (status) => {
      validateManagedReporterStatus(status, {
        ...loadPlan,
        minimumReports: recoveredStatus.successfulReports + 1,
        requireAbuse: true,
      });
      return true;
    },
    180,
  );

  await A.setAdScoringPaused(current.gameId, true);
  const finalEvidence = await waitUntil(
    'final managed KotH integrity',
    async () => integritySnapshot(),
    (evidence) => {
      validateManagedKothIntegrity(evidence, {
        rosterSize: ROSTER_SIZE,
        activeFleet: ACTIVE_FLEET,
        minimumScorableRounds: 4,
        minimumResetAttempts: target.resetAttempt,
      });
      return true;
    },
    180,
  );
  validateManagedKothIntegrity(finalEvidence, {
    rosterSize: ROSTER_SIZE,
    activeFleet: ACTIVE_FLEET,
    minimumScorableRounds: 4,
    minimumResetAttempts: target.resetAttempt,
  });
  console.log(`managed KotH acceptance passed for hidden game ${current.gameId}`);
  console.log(`fixed-rate summaries: ${resolve(config.summaryPath)}, ${suffixPath(config.summaryPath, 'prefix')}, ${suffixPath(config.summaryPath, 'restart')}, ${suffixPath(config.summaryPath, 'abuse')}`);
  console.log(`resource series: ${resolve(config.resourcePath)}`);
}

let failure;
try {
  await main();
} catch (error) {
  failure = error;
} finally {
  try {
    writeFileSync(resolve(config.resourcePath), `${JSON.stringify({
      schemaVersion: 1,
      rosterSize: ROSTER_SIZE,
      activeFleet: ACTIVE_FLEET,
      fixedArrivalProfiles: {
        valid: { rate: loadPlan.rate, durationSeconds: loadPlan.durationSeconds },
        abuse: { rate: abusePlan.rate, durationSeconds: abusePlan.durationSeconds },
      },
      phases: resourcePhases,
    }, null, 2)}\n`, { mode: 0o600 });
  } catch (resourceError) {
    failure = failure
      ? new AggregateError([failure, resourceError], 'managed KotH scenario and resource retention failed')
      : resourceError;
  }
  try {
    if (gameIds.length > 0) await A.teardownNamespace(gameIds);
  } catch (cleanupError) {
    failure = failure
      ? new AggregateError([failure, cleanupError], 'managed KotH scenario and cleanup failed')
      : cleanupError;
  }
  rmSync(tokenSandbox, { recursive: true, force: true });
  if (processLock) await processLock.release();
  process.off('SIGINT', onInterrupt);
  process.off('SIGTERM', onInterrupt);
}

if (failure) throw failure;
