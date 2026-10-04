// Stress a prepared, disposable Queued challenge through its real first-start
// path. The runner never fabricates source archives or mutates build status.
import { execFileSync, spawn } from 'node:child_process';
import { writeFileSync } from 'node:fs';

import { mintJwt, PG, RSCTF, sql, TARGET } from './lib.mjs';
import {
  parseDockerStat,
  parseFilesystemStat,
  parseProcessStat,
  summarizeResourceSamples,
} from './image-storage.js';

const gameId = positiveInteger(process.env.GAME, 'GAME');
const challengeId = positiveInteger(process.env.CID, 'CID');
const requestedUsers = positiveInteger(process.env.N || 8, 'N', 256);
const target = new URL(TARGET);
const acknowledged = process.env.IMAGE_STORAGE_STRESS_ACK === '1';
const remoteAcknowledgement = process.env.ALLOW_REMOTE_IMAGE_STORAGE_STRESS;
const closeoutGameId = process.env.IMAGE_STORAGE_CLOSEOUT_GAME
  ? positiveInteger(process.env.IMAGE_STORAGE_CLOSEOUT_GAME, 'IMAGE_STORAGE_CLOSEOUT_GAME')
  : null;
const hungDocker = process.env.IMAGE_STORAGE_HUNG_DOCKER_ACK === 'external-fault-proxy';
const expectedCleanupBacklog = Number(process.env.EXPECTED_CLEANUP_BACKLOG_MIN || 0);
const expectedCleanupMinMs = Number(process.env.EXPECTED_CLEANUP_MIN_MS || 0);
const backendProcessId = process.env.RSCTF_PROCESS_PID
  ? positiveInteger(process.env.RSCTF_PROCESS_PID, 'RSCTF_PROCESS_PID')
  : null;
if (!acknowledged) throw new Error('set IMAGE_STORAGE_STRESS_ACK=1 for this destructive container-start test');
if (!['127.0.0.1', 'localhost', '::1'].includes(target.hostname) && remoteAcknowledgement !== target.origin) {
  throw new Error(`remote TARGET requires ALLOW_REMOTE_IMAGE_STORAGE_STRESS=${target.origin}`);
}
if (closeoutGameId && process.env.IMAGE_STORAGE_CLOSEOUT_ACK !== String(closeoutGameId)) {
  throw new Error(`event-closeout probes require IMAGE_STORAGE_CLOSEOUT_ACK=${closeoutGameId}`);
}
if (!Number.isSafeInteger(expectedCleanupBacklog) || expectedCleanupBacklog < 0) {
  throw new Error('EXPECTED_CLEANUP_BACKLOG_MIN must be a non-negative integer');
}
if (
  !Number.isSafeInteger(expectedCleanupMinMs) || expectedCleanupMinMs < 0 ||
  expectedCleanupMinMs > 120_000 || (expectedCleanupMinMs > 0 && !hungDocker)
) {
  throw new Error(
    'EXPECTED_CLEANUP_MIN_MS must be 0..120000 and requires IMAGE_STORAGE_HUNG_DOCKER_ACK=external-fault-proxy',
  );
}

function positiveInteger(value, label, maximum = Number.MAX_SAFE_INTEGER) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0 || parsed > maximum) {
    throw new Error(`${label} must be an integer from 1 through ${maximum}`);
  }
  return parsed;
}

function jsonQuery(query) {
  const value = sql(query);
  if (!value) throw new Error('fixture query returned no row');
  return JSON.parse(value);
}

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

function scheduleSnapshot(scope) {
  if (!/^[a-f0-9]{32}$/.test(scope)) throw new Error(`invalid installation scope ${scope}`);
  return jsonQuery(
    `SELECT json_build_object(` +
      `'scope',installation_scope,'leaseToken',lease_token,` +
      `'nextRunMs',floor(extract(epoch from next_run_at_utc)*1000)::bigint,` +
      `'lastStartedMs',floor(extract(epoch from last_started_at_utc)*1000)::bigint,` +
      `'lastFinishedMs',floor(extract(epoch from last_finished_at_utc)*1000)::bigint,` +
      `'lastScanned',last_scanned,'lastClaimed',last_claimed,'lastRemoved',last_removed,` +
      `'lastBacklog',last_backlog,'lastDurationMs',last_duration_ms,'lastError',last_error` +
    `)::text FROM "ImageCleanupSchedules" WHERE installation_scope='${scope}'`,
  );
}

async function waitForScheduledCleanup(scope, previousStartedMs, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let observedLease = false;
  while (Date.now() < deadline) {
    const current = scheduleSnapshot(scope);
    observedLease ||= typeof current.leaseToken === 'string' && current.leaseToken.length > 0;
    if (
      Number(current.lastStartedMs) > Number(previousStartedMs || 0) &&
      Number(current.lastFinishedMs) >= Number(current.lastStartedMs) &&
      current.leaseToken === null
    ) {
      return { ...current, observedLease };
    }
    await sleep(100);
  }
  throw new Error(`scheduled image cleanup did not finish within ${timeoutMs}ms`);
}

function assertScheduleReport(schedule) {
  for (const field of ['lastScanned', 'lastClaimed', 'lastRemoved', 'lastBacklog', 'lastDurationMs']) {
    if (!Number.isSafeInteger(Number(schedule[field])) || Number(schedule[field]) < 0) {
      throw new Error(`scheduled cleanup returned invalid ${field}: ${JSON.stringify(schedule)}`);
    }
  }
  if (schedule.lastClaimed > 32 || schedule.lastClaimed > schedule.lastScanned) {
    throw new Error(`scheduled cleanup exceeded its candidate claim bound: ${JSON.stringify(schedule)}`);
  }
  if (schedule.lastRemoved > schedule.lastClaimed) {
    throw new Error(`scheduled cleanup removed more images than it claimed: ${JSON.stringify(schedule)}`);
  }
  if (schedule.lastBacklog < expectedCleanupBacklog) {
    throw new Error(
      `scheduled cleanup backlog ${schedule.lastBacklog} is below expected floor ${expectedCleanupBacklog}`,
    );
  }
  if (Number(schedule.nextRunMs) <= Number(schedule.lastStartedMs)) {
    throw new Error(`scheduled cleanup did not durably advance its cadence: ${JSON.stringify(schedule)}`);
  }
  const elapsedMs = Number(schedule.lastFinishedMs) - Number(schedule.lastStartedMs);
  if (!Number.isSafeInteger(elapsedMs) || elapsedMs < expectedCleanupMinMs || elapsedMs > 125_000) {
    throw new Error(`scheduled cleanup wall duration ${elapsedMs}ms is outside the asserted bound`);
  }
  if (!hungDocker && schedule.lastError !== null) {
    throw new Error(`scheduled cleanup failed unexpectedly: ${schedule.lastError}`);
  }
  if (
    hungDocker && schedule.lastError !== null &&
    !/(timed out|deadline|budget)/i.test(String(schedule.lastError))
  ) {
    throw new Error(`fault-proxy cleanup failed for an unexpected reason: ${schedule.lastError}`);
  }
}

const fixture = jsonQuery(
  `SELECT json_build_object(` +
    `'gameId',game.id,'challengeId',challenge.id,'type',challenge."Type",` +
    `'status',challenge.build_status,'digest',challenge.build_image_digest,` +
    `'archive',challenge.original_archive_blob_path,'context',challenge.build_context_subdir,` +
    `'workload',challenge.workload_spec,'enabled',challenge.is_enabled,` +
    `'review',challenge.review_status,'live',` +
      `(game.start_time_utc<=clock_timestamp() AND game.end_time_utc>=clock_timestamp())` +
  `)::text FROM "Games" game JOIN "GameChallenges" challenge ON challenge.game_id=game.id ` +
  `WHERE game.id=${gameId} AND challenge.id=${challengeId}`,
);
if (
  !fixture.live || !fixture.enabled || fixture.review !== 0 || ![1, 3].includes(fixture.type) ||
  fixture.status !== 5 || fixture.digest !== null || !fixture.archive || !fixture.context || fixture.workload !== null
) {
  throw new Error(`challenge is not a live, enabled, archive-backed Queued Jeopardy fixture: ${JSON.stringify(fixture)}`);
}
const lazyEnabled = sql(
  `SELECT COALESCE((SELECT lower(value)='true' FROM "Configs" ` +
    `WHERE config_key='ContainerPolicy:BuildImagesOnDemand'),FALSE)`,
);
if (lazyEnabled !== 't') throw new Error('ContainerPolicy:BuildImagesOnDemand must be enabled');

const players = jsonQuery(
  `SELECT COALESCE(json_agg(row_to_json(candidate) ORDER BY candidate.participation_id),'[]'::json)::text FROM (` +
    `SELECT DISTINCT ON (participation.id) participation.id AS participation_id, ` +
      `participation.team_id, account.id AS user_id, account.security_stamp, account.role ` +
    `FROM "Participations" participation ` +
    `JOIN "UserParticipations" link ON link.participation_id=participation.id ` +
      `AND link.game_id=participation.game_id AND link.team_id=participation.team_id ` +
    `JOIN "AspNetUsers" account ON account.id=link.user_id ` +
    `JOIN "Teams" team ON team.id=participation.team_id ` +
    `LEFT JOIN "TeamMembers" member ON member.team_id=team.id AND member.user_id=account.id ` +
    `WHERE participation.game_id=${gameId} AND participation.status=1 ` +
      `AND account.email_confirmed AND account.role=1 AND team.deletion_pending=FALSE ` +
      `AND (team.captain_id=account.id OR member.user_id IS NOT NULL) ` +
      `AND EXISTS (SELECT 1 FROM "IdentityObservations" observation ` +
        `WHERE observation.user_id=account.id AND observation.game_id=participation.game_id ` +
          `AND observation.team_id=participation.team_id ` +
          `AND observation.participation_id=participation.id ` +
          `AND observation.observed_at_utc<=clock_timestamp()) ` +
    `ORDER BY participation.id, (team.captain_id=account.id) DESC, account.id ` +
    `LIMIT ${requestedUsers}` +
  `) candidate`,
);
if (players.length < 2) throw new Error(`fixture has only ${players.length} eligible distinct participation(s)`);

const existing = Number(sql(
  `SELECT count(*) FROM "GameInstances" instance JOIN "Containers" container ` +
    `ON container.id=instance.container_id WHERE instance.challenge_id=${challengeId}`,
));
if (existing !== 0) throw new Error(`fixture already has ${existing} materialized container(s)`);

const tokens = players.map((player) => mintJwt(player.user_id, player.security_stamp, player.role));
const beforeBuilds = Number(sql(
  `SELECT count(*) FROM "BuildRecords" WHERE challenge_id=${challengeId} AND trigger='RuntimeStart'`,
));
if (beforeBuilds !== 0) {
  throw new Error(`fixture already has ${beforeBuilds} RuntimeStart build record(s)`);
}
const context = JSON.stringify({ gameId, challengeId, tokens });
const k6Path = new URL('./k6/image-storage.js', import.meta.url).pathname;
const summaryPath = process.env.SUMMARY_JSON || `/tmp/rsctf-image-storage-${Date.now()}.json`;
const cleanupSummaryPath = process.env.CLEANUP_SUMMARY_JSON || summaryPath.replace(/\.json$/, '-cleanup.json');
const resourcePath = process.env.RESOURCE_JSON || summaryPath.replace(/\.json$/, '-resources.json');
const samples = [];
let sampling = false;

async function sample() {
  if (sampling) return;
  sampling = true;
  try {
    const health = await fetch(new URL('/healthz', target), { signal: AbortSignal.timeout(3000) });
    const body = await health.text();
    const command = dockerStats(backendProcessId ? [PG] : [RSCTF, PG]);
    if (backendProcessId) {
      const processStat = execFileSync(
        'ps', ['-p', String(backendProcessId), '-o', '%cpu=,rss='], { encoding: 'utf8' },
      );
      command.push(parseProcessStat(processStat, backendProcessId));
    }
    const filesystem = parseFilesystemStat(
      execFileSync('df', ['-B1', '--output=size,avail', '/'], { encoding: 'utf8' }),
    );
    samples.push({
      at: Date.now(),
      healthStatus: health.status,
      healthBody: body,
      filesystem,
      resources: command.map((row) => typeof row === 'string' ? parseDockerStat(row) : row),
    });
  } catch (error) {
    samples.push({ at: Date.now(), healthStatus: 0, healthBody: String(error), resources: [] });
  } finally {
    sampling = false;
  }
}

function dockerStats(containers) {
  const result = execFileSync('docker', [
    'stats', '--no-stream', '--format', '{{.Name}}|{{.CPUPerc}}|{{.MemUsage}}', ...containers,
  ], { encoding: 'utf8' });
  return result.trim().split('\n').filter(Boolean).map((line) => {
    const [name, cpu, memory] = line.split('|');
    return `${name}|${cpu}|${memory.split('/')[0].trim()}`;
  });
}

async function runK6(summary, extraEnv = {}) {
  const child = spawn('k6', ['run', '--summary-export', summary, k6Path], {
    stdio: 'inherit',
    env: {
      ...process.env,
      TARGET: target.origin,
      IMAGE_STORAGE_CONTEXT: context,
      ...extraEnv,
    },
  });
  const status = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code) => resolve(code ?? 1));
  });
  if (status !== 0) throw new Error(`k6 image-storage ${extraEnv.IMAGE_STORAGE_PHASE || 'build'} phase failed with exit status ${status}`);
}

await sample();
const timer = setInterval(sample, 2000);
await runK6(summaryPath, { IMAGE_STORAGE_PHASE: 'build' });

const audit = jsonQuery(
  `SELECT json_build_object(` +
    `'runtimeBuilds',(SELECT count(*) FROM "BuildRecords" WHERE challenge_id=${challengeId} ` +
      `AND trigger='RuntimeStart'),` +
    `'successfulBuilds',(SELECT count(*) FROM "BuildRecords" WHERE challenge_id=${challengeId} ` +
      `AND trigger='RuntimeStart' AND status=1),` +
    `'status',challenge.build_status,'digest',challenge.build_image_digest,` +
    `'lastUsed',(SELECT max(last_used_at_utc) FROM "BuildImageOwnerships" ownership ` +
      `WHERE ownership.image_id=challenge.build_image_digest),` +
    `'containers',(SELECT count(DISTINCT instance.participation_id) FROM "GameInstances" instance ` +
      `JOIN "Containers" container ON container.id=instance.container_id ` +
      `WHERE instance.challenge_id=challenge.id AND container.status=1)` +
  `)::text FROM "GameChallenges" challenge WHERE challenge.id=${challengeId}`,
);
if (
  audit.runtimeBuilds !== beforeBuilds + 1 || audit.successfulBuilds !== beforeBuilds + 1 ||
  audit.status !== 1 || !/^sha256:[a-f0-9]{64}$/.test(audit.digest || '') ||
  !audit.lastUsed || audit.containers !== players.length
) {
  throw new Error(`on-demand build audit failed: ${JSON.stringify({ beforeBuilds, players: players.length, audit })}`);
}

let cleanup = null;
let closeout = null;
if (process.env.IMAGE_STORAGE_SKIP_CLEANUP !== '1') {
  const admin = jsonQuery(
    `SELECT json_build_object('id',id,'stamp',security_stamp,'role',role)::text ` +
      `FROM "AspNetUsers" WHERE role=3 AND email_confirmed ORDER BY id LIMIT 1`,
  );
  const adminToken = process.env.ADMIN_TOKEN || mintJwt(admin.id, admin.stamp, admin.role);
  if (closeoutGameId) {
    closeout = jsonQuery(
      `SELECT json_build_object(` +
        `'gameId',game.id,'ended',game.end_time_utc<clock_timestamp(),` +
        `'rounds',(SELECT count(*) FROM "AdRounds" round WHERE round.game_id=game.id),` +
        `'pending',(SELECT count(*) FROM "AdRounds" round WHERE round.game_id=game.id ` +
          `AND (round.finalized=FALSE OR round.pipeline_completed_at IS NULL))` +
      `)::text FROM "Games" game WHERE game.id=${closeoutGameId}`,
    );
    if (!closeout.ended || closeout.rounds < 1) {
      throw new Error(`IMAGE_STORAGE_CLOSEOUT_GAME is not a disposable ended A&D fixture: ${JSON.stringify(closeout)}`);
    }
  }

  if (sql(`SELECT to_regclass('"ImageCleanupSchedules"') IS NOT NULL`) !== 't') {
    throw new Error('ImageCleanupSchedules migration is not installed');
  }
  const scopes = jsonQuery(
    `SELECT COALESCE(json_agg(scope),'[]'::json)::text FROM (` +
      `SELECT DISTINCT installation_scope AS scope FROM "BuildImageOwnerships" ` +
      `WHERE image_id='${audit.digest}' ORDER BY installation_scope LIMIT 2` +
    `) owned`,
  );
  if (scopes.length !== 1 || !/^[a-f0-9]{32}$/.test(scopes[0])) {
    throw new Error(`built image did not resolve to one installation scope: ${JSON.stringify(scopes)}`);
  }
  const scope = scopes[0];
  sql(
    `INSERT INTO "ImageCleanupSchedules" (installation_scope) VALUES ('${scope}') ` +
    `ON CONFLICT (installation_scope) DO NOTHING`,
  );
  let baseline = scheduleSnapshot(scope);
  const idleDeadline = Date.now() + 125_000;
  while (baseline.leaseToken !== null && Date.now() < idleDeadline) {
    await sleep(100);
    baseline = scheduleSnapshot(scope);
  }
  if (baseline.leaseToken !== null) throw new Error('an existing image cleanup lease did not finish');

  const probeSeconds = positiveInteger(
    process.env.CLEANUP_PROBE_SECONDS || (hungDocker ? 125 : 65),
    'CLEANUP_PROBE_SECONDS',
    180,
  );
  const cleanupProbe = runK6(cleanupSummaryPath, {
    IMAGE_STORAGE_PHASE: 'cleanup',
    HEALTH_DURATION: `${probeSeconds}s`,
    ...(closeoutGameId ? {
      IMAGE_STORAGE_CLOSEOUT_GAME: String(closeoutGameId),
      IMAGE_STORAGE_CLOSEOUT_TOKEN: adminToken,
    } : {}),
  });
  await sleep(500);
  sql(
    `UPDATE "ImageCleanupSchedules" SET next_run_at_utc=clock_timestamp(), ` +
      `updated_at_utc=clock_timestamp() WHERE installation_scope='${scope}' ` +
      `AND (lease_until IS NULL OR lease_until<=clock_timestamp())`,
  );
  const schedule = waitForScheduledCleanup(
    scope,
    baseline.lastStartedMs,
    Math.max(35_000, probeSeconds * 1000 - 5_000),
  );
  [cleanup] = await Promise.all([schedule, cleanupProbe]);
  assertScheduleReport(cleanup);

  // The probe spans multiple 30-second scheduler ticks. An unchanged start and
  // next-run timestamp proves a restart/failover contender observes the durable
  // cadence instead of rerunning cleanup from a process-local zero timestamp.
  const stable = scheduleSnapshot(scope);
  if (
    stable.lastStartedMs !== cleanup.lastStartedMs || stable.nextRunMs !== cleanup.nextRunMs ||
    stable.leaseToken !== null
  ) {
    throw new Error(`scheduled cleanup cadence was not stable across probe ticks: ${JSON.stringify({ cleanup, stable })}`);
  }
  cleanup.stableAfterFixedRateProbe = true;

  if (closeoutGameId) {
    const after = jsonQuery(
      `SELECT json_build_object(` +
        `'pending',(SELECT count(*) FROM "AdRounds" WHERE game_id=${closeoutGameId} ` +
          `AND (finalized=FALSE OR pipeline_completed_at IS NULL)),` +
        `'completedAtMs',floor(extract(epoch from max(pipeline_completed_at))*1000)::bigint` +
      `)::text FROM "AdRounds" WHERE game_id=${closeoutGameId}`,
    );
    if (after.pending > closeout.pending) {
      throw new Error(`event closeout regressed during image cleanup: ${JSON.stringify({ closeout, after })}`);
    }
    closeout = { ...closeout, ...after, fixedRateEndpoint: `/api/Game/${closeoutGameId}/Ad/Scoreboard` };
  }
}

clearInterval(timer);
while (sampling) await new Promise((resolve) => setTimeout(resolve, 25));
await sample();

const resources = summarizeResourceSamples(samples);
if (resources.healthFailures !== 0) throw new Error(`health failed during stress: ${JSON.stringify(resources)}`);
writeFileSync(
  resourcePath,
  `${JSON.stringify({ fixture, players: players.length, audit, cleanup, closeout, resources, samples }, null, 2)}\n`,
);
console.log(`image_storage_ok builds=1 containers=${audit.containers} samples=${resources.samples}`);
console.log(`summary=${summaryPath}`);
if (cleanup) console.log(`cleanup_summary=${cleanupSummaryPath}`);
console.log(`resources=${resourcePath}`);
