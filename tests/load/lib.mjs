// Shared helpers for the load tests: config, docker/psql shells, token minting, and
// service discovery. Node orchestration (this file + the *.mjs scenarios) sets up state
// and runs the k6 HTTP scenarios in ./k6.
import { execFileSync, spawnSync } from 'node:child_process';
import crypto from 'node:crypto';

export const TARGET = process.env.TARGET || 'http://127.0.0.1:8080';
export const GAME = process.env.GAME || '10';
export const PG = process.env.PG_CONTAINER || 'rsctf-db-1';
export const PG_USER = process.env.PG_USER || 'postgres';
export const PG_DATABASE = process.env.PG_DATABASE || 'rsctf';
export const RSCTF = process.env.RSCTF_CONTAINER || 'rsctf-rsctf-1';
export const NET = process.env.NET || 'rsctf_default';
export const DEFAULT_BYOC_AGENT_IMAGE =
  'ghcr.io/dimasma0305/rsctf-byoc-agent@sha256:fa5243f7aea7cd1198668134f5c1bae99c339c773ba3a5902d633c2c56c6c490';
export const JWT_SECRET = process.env.RSCTF_JWT_SECRET;
if (!JWT_SECRET) throw new Error('RSCTF_JWT_SECRET is required for load-test token minting');

const b64url = (b) => Buffer.from(b).toString('base64url');

/** Run a psql query in the DB container, return trimmed stdout. */
export function sql(query) {
  // Quiet mode suppresses INSERT/UPDATE/DELETE command tags. Callers compare
  // RETURNING output as an exact identity; `uuid\nINSERT 0 1` is not a UUID.
  return execFileSync('docker', ['exec', PG, 'psql', '-U', PG_USER, '-d', PG_DATABASE, '-qAtc', query], {
    encoding: 'utf8',
  }).trim();
}

/** docker CLI; returns { status, stdout, stderr }. */
export function docker(args, opts = {}) {
  return spawnSync('docker', args, { encoding: 'utf8', ...opts });
}

function containerEnvVar(name) {
  try {
    const envList = JSON.parse(
      execFileSync('docker', ['inspect', '-f', '{{json .Config.Env}}', name], { encoding: 'utf8' }).trim(),
    );
    return Array.isArray(envList) ? envList : [];
  } catch {
    return [];
  }
}

function containerRole(name) {
  const env = containerEnvVar(name);
  for (const entry of env) {
    const [key, value] = String(entry).split('=');
    if (key === 'RSCTF_ROLE') return (value || '').toLowerCase();
  }
  return '';
}

function containerProject(name) {
  try {
    const value = execFileSync('docker', ['inspect', '-f', '{{index .Config.Labels "com.docker.compose.project"}}', name], {
      encoding: 'utf8',
    }).trim();
    return value || '';
  } catch {
    return '';
  }
}

function candidatesFromProject(project) {
  if (!project) return [];
  try {
    const output = execFileSync(
      'docker',
      ['ps', '--filter', `label=com.docker.compose.project=${project}`, '--format', '{{.Names}}'],
      { encoding: 'utf8' },
    )
      .trim()
      .split('\n')
      .map((line) => line.trim())
      .filter(Boolean);
    return output;
  } catch {
    return [];
  }
}

function containerExists(name) {
  try {
    execFileSync('docker', ['inspect', name], { encoding: 'utf8' });
    return true;
  } catch {
    return false;
  }
}

function resolveByocContainer() {
  const candidates = [
    process.env.RSCTF_BYOC_CONTAINER,
    process.env.RSCTF_CONTROL_CONTAINER,
    process.env.RSCTF_CONTROL,
    RSCTF,
  ].filter((value, index, all) => value && all.indexOf(value) === index);

  for (const candidate of candidates) {
    if (!containerExists(candidate)) continue;
    if (containerRole(candidate) !== 'web') return candidate;
  }

  const project = containerProject(RSCTF);
  const projectContainers = candidatesFromProject(project);
  for (const candidate of projectContainers) {
    if (!candidate || candidate === RSCTF) continue;
    if (!containerExists(candidate)) continue;
    if (containerRole(candidate) !== 'web') return candidate;
  }

  return candidates[0] || RSCTF;
}

const BYOC_CONTAINER = resolveByocContainer();

/** rsctf container's IP on NET (for agents to dial ws://IP:8080). */
export function rsctfIp(container = RSCTF) {
  return execFileSync('docker', ['inspect', container, '-f', `{{(index .NetworkSettings.Networks "${NET}").IPAddress}}`], {
    encoding: 'utf8',
  }).trim();
}

/** rsctf container that owns stateful/BYOC routes (falls back to RSCTF). */
export function byocRsctfIp() {
  return rsctfIp(BYOC_CONTAINER);
}

/** One rsctf resource sample: { cpu: number%, mem: 'NNMiB' }. */
export function stat() {
  const s = execFileSync('docker', ['stats', '--no-stream', '--format', '{{.CPUPerc}}|{{.MemUsage}}', RSCTF], {
    encoding: 'utf8',
  }).trim();
  const [cpu, mem] = s.split('|');
  return { cpu: parseFloat(cpu), mem: mem.split('/')[0].trim() };
}

/** HS256 JWT bound to the live security stamp. Role defaults to Admin for setup helpers. */
export function mintJwt(userId, securityStamp, role = 3) {
  if (!Number.isSafeInteger(role) || role < 0 || role > 3) {
    throw new Error(`invalid load-test JWT role ${role}`);
  }
  const stamp = securityStamp ?? sql(`SELECT security_stamp FROM "AspNetUsers" WHERE id='${userId}'::uuid`);
  const seg =
    b64url(JSON.stringify({ alg: 'HS256', typ: 'JWT' })) +
    '.' +
    b64url(
      JSON.stringify({
        sub: userId,
        role,
        name: 'player',
        stamp,
        iat: (Date.now() / 1000) | 0,
        exp: ((Date.now() / 1000) | 0) + 7200,
      })
    );
  return seg + '.' + b64url(crypto.createHmac('sha256', JWT_SECRET).update(seg).digest());
}

/** Deterministic BYOC agent token, bound to both the game and team secrets. */
export function byocToken(pid, cid, gameSecret, teamSecret) {
  const h = crypto.createHash('sha256');
  h.update('adbyocagent:');
  h.update(gameSecret);
  h.update(teamSecret);
  const b = Buffer.alloc(8);
  b.writeInt32LE(pid, 0);
  b.writeInt32LE(cid, 4);
  h.update(b.subarray(0, 4));
  h.update(b.subarray(4, 8));
  return h.digest('hex');
}

const positiveInt = (value, label) => {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0)
    throw new Error(`${label} must be a positive integer (got ${value})`);
  return parsed;
};

function liveByocGrantRows(gameId, challengeId) {
  const gid = positiveInt(gameId, 'game id');
  const cid = positiveInt(challengeId, 'BYOC challenge id');
  const gameSecret = sql(
    `SELECT private_key FROM "Games" WHERE id=${gid} AND start_time_utc<=now() AND now()<=end_time_utc`
  );
  if (!gameSecret) throw new Error(`BYOC game ${gid} does not exist or is not currently live`);

  const challenge = sql(
    `SELECT id FROM "GameChallenges" WHERE id=${cid} AND game_id=${gid} AND "Type"=4 ` +
      `AND ad_self_hosted AND is_enabled AND review_status=0`
  );
  if (!challenge) {
    throw new Error(`challenge ${cid} is not an enabled, Active self-hosted A&D challenge in live game ${gid}`);
  }

  const raw = sql(
    `SELECT COALESCE(json_agg(json_build_object('pid',p.id,'teamSecret',t.invite_token) ORDER BY p.id),'[]'::json)::text ` +
      `FROM "Participations" p JOIN "Teams" t ON t.id=p.team_id ` +
      `WHERE p.game_id=${gid} AND p.status=1 AND t.invite_token IS NOT NULL AND t.invite_token<>''`
  );
  const participants = JSON.parse(raw || '[]');
  return { gid, cid, gameSecret, participants };
}

const deriveByocCapability = ({ pid, teamSecret }, cid, gameSecret) => ({
  pid: Number(pid),
  cid,
  token: byocToken(Number(pid), cid, gameSecret, teamSecret),
});

/** Resolve N distinct live Accepted participants into in-memory BYOC capabilities. */
export function byocCapabilities(count, challengeId, gameId = GAME) {
  const required = positiveInt(count, 'BYOC participant count');
  const { gid, cid, gameSecret, participants } = liveByocGrantRows(gameId, challengeId);
  if (participants.length < required) {
    throw new Error(
      `BYOC fleet requires ${required} distinct Accepted participations in live game ${gid}, but only ` +
        `${participants.length} are available; provision more real teams or reduce N/FLEET (synthetic IDs are not authorized)`
    );
  }
  return participants.slice(0, required).map((row) => deriveByocCapability(row, cid, gameSecret));
}

/** Resolve a specified set of participation IDs, rejecting stale/non-Accepted IDs. */
export function byocCapabilitiesForPids(participationIds, challengeId, gameId = GAME) {
  if (!Array.isArray(participationIds) || participationIds.length === 0) {
    throw new Error('at least one real participation id is required for a BYOC fleet');
  }
  const requested = participationIds.map((pid) => positiveInt(pid, 'participation id'));
  if (new Set(requested).size !== requested.length) throw new Error('BYOC participation ids must be distinct');

  const { gid, cid, gameSecret, participants } = liveByocGrantRows(gameId, challengeId);
  const byPid = new Map(participants.map((row) => [Number(row.pid), row]));
  const missing = requested.filter((pid) => !byPid.has(pid));
  if (missing.length) {
    throw new Error(
      `BYOC fleet requested ${requested.length} participations in game ${gid}, but ${missing.length} are missing, ` +
        `revoked, or not Accepted (ids: ${missing.slice(0, 10).join(',')}${missing.length > 10 ? ',…' : ''})`
    );
  }
  return requested.map((pid) => deriveByocCapability(byPid.get(pid), cid, gameSecret));
}

/** Discover the game's players/hills/challenges from the running stack. */
export function discover() {
  const users = (
    sql(
      `SELECT string_agg(up.user_id::text || '|' || u.security_stamp, ',') FROM "UserParticipations" up JOIN "Participations" p ON p.id = up.participation_id JOIN "AspNetUsers" u ON u.id=up.user_id WHERE p.game_id = ${GAME} AND p.status = 1`
    ) || ''
  )
    .split(',')
    .filter(Boolean);
  return {
    tokens: users.map((entry) => {
      const [id, stamp] = entry.split('|');
      return mintJwt(id, stamp, 1);
    }),
    kothHills:
      sql(
        `SELECT string_agg(id::text, ',') FROM "GameChallenges" WHERE game_id = ${GAME} AND "Type" = 5 AND is_enabled`
      ) || '',
    adChals:
      sql(`SELECT string_agg(DISTINCT challenge_id::text, ',') FROM "AdTeamServices" WHERE game_id = ${GAME}`) || '',
    byocChal:
      process.env.CID ||
      sql(
        `SELECT id FROM "GameChallenges" WHERE game_id=${GAME} AND ad_self_hosted AND "Type"=4 ` +
          `AND is_enabled AND review_status=0 ORDER BY id LIMIT 1`
      ),
  };
}

/** Run a k6 script under ./k6 with env, streaming its output. Returns exit code. */
export function runK6(script, env = {}) {
  const args = ['run'];
  const summaryJson = env.SUMMARY_JSON || process.env.SUMMARY_JSON;
  if (summaryJson) args.push('--summary-export', summaryJson);
  args.push(new URL(`./k6/${script}`, import.meta.url).pathname);
  const r = spawnSync('k6', args, {
    stdio: 'inherit',
    encoding: 'utf8',
    env: { ...process.env, ...Object.fromEntries(Object.entries(env).map(([k, v]) => [k, String(v)])) },
  });
  return r.status ?? 1;
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
