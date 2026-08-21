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
const backendProcessId = process.env.RSCTF_PROCESS_PID
  ? positiveInteger(process.env.RSCTF_PROCESS_PID, 'RSCTF_PROCESS_PID')
  : null;
if (!acknowledged) throw new Error('set IMAGE_STORAGE_STRESS_ACK=1 for this destructive container-start test');
if (!['127.0.0.1', 'localhost', '::1'].includes(target.hostname) && remoteAcknowledgement !== target.origin) {
  throw new Error(`remote TARGET requires ALLOW_REMOTE_IMAGE_STORAGE_STRESS=${target.origin}`);
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

await sample();
const timer = setInterval(sample, 2000);
const child = spawn('k6', ['run', '--summary-export', summaryPath, k6Path], {
  stdio: 'inherit',
  env: {
    ...process.env,
    TARGET: target.origin,
    IMAGE_STORAGE_CONTEXT: context,
  },
});
const status = await new Promise((resolve, reject) => {
  child.once('error', reject);
  child.once('exit', (code) => resolve(code ?? 1));
});
if (status !== 0) throw new Error(`k6 image-storage stress failed with exit status ${status}`);

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
if (process.env.IMAGE_STORAGE_SKIP_CLEANUP !== '1') {
  const admin = jsonQuery(
    `SELECT json_build_object('id',id,'stamp',security_stamp,'role',role)::text ` +
      `FROM "AspNetUsers" WHERE role=3 AND email_confirmed ORDER BY id LIMIT 1`,
  );
  const adminToken = process.env.ADMIN_TOKEN || mintJwt(admin.id, admin.stamp, admin.role);
  const cleanupStartedAt = Date.now();
  const response = await fetch(new URL('/api/admin/builds/prunestorage', target), {
    method: 'POST',
    headers: { Authorization: `Bearer ${adminToken}` },
    signal: AbortSignal.timeout(180_000),
  });
  const body = await response.text();
  if (response.status !== 200) throw new Error(`storage cleanup returned ${response.status}: ${body}`);
  cleanup = JSON.parse(body);
  cleanup.durationMs = Date.now() - cleanupStartedAt;
  if (!Number.isSafeInteger(cleanup.imagesRemoved) || cleanup.imagesRemoved < 0) {
    throw new Error(`storage cleanup returned an invalid report: ${body}`);
  }
}

clearInterval(timer);
while (sampling) await new Promise((resolve) => setTimeout(resolve, 25));
await sample();

const resources = summarizeResourceSamples(samples);
if (resources.healthFailures !== 0) throw new Error(`health failed during stress: ${JSON.stringify(resources)}`);
writeFileSync(
  resourcePath,
  `${JSON.stringify({ fixture, players: players.length, audit, cleanup, resources, samples }, null, 2)}\n`,
);
console.log(`image_storage_ok builds=1 containers=${audit.containers} samples=${resources.samples}`);
console.log(`summary=${summaryPath}`);
console.log(`resources=${resourcePath}`);
