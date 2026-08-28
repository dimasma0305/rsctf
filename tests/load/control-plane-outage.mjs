// Explicit worker-process and prepared missing-image recovery gate.
import { randomUUID } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

import { isBoundedImageFailure, validHealthyProxyEndpoints, validWorkerInventory } from './control-plane-outage.js';
import { mintJwt, runK6, sleep, sql, TARGET } from './lib.mjs';

const workerId = String(process.env.OUTAGE_WORKER_ID || '').toLowerCase();
const workerContainer = String(process.env.OUTAGE_WORKER_CONTAINER || '');
const proxyEndpointsFile = String(process.env.HEALTHY_PROXY_ENDPOINTS_FILE || '');
const admin = sql(
  `SELECT id::text || '|' || security_stamp || '|' || role::text FROM "AspNetUsers" ` +
    `WHERE role=3 AND security_stamp IS NOT NULL ORDER BY id LIMIT 1`,
);
if (!admin) throw new Error('one Admin account is required');
const [adminId, adminStamp, adminRole] = admin.split('|');
const adminToken = process.env.ADMIN_TOKEN || mintJwt(adminId, adminStamp, Number(adminRole));
if (!/^[0-9a-f-]{36}$/.test(workerId)) throw new Error('OUTAGE_WORKER_ID is required');

async function api(path, options = {}) {
  const response = await fetch(new URL(path, TARGET), {
    ...options,
    headers: { Authorization: `Bearer ${adminToken}`, ...(options.headers || {}) },
    signal: AbortSignal.timeout(options.timeoutMs || 60_000),
  });
  const text = await response.text();
  let body;
  try { body = JSON.parse(text); } catch { body = null; }
  return { response, text, body };
}

async function waitFor(check, label, timeoutMs = 90_000) {
  const deadline = Date.now() + timeoutMs;
  let last;
  while (Date.now() < deadline) {
    last = await check();
    if (last?.ok) return last;
    await sleep(500);
  }
  throw new Error(`${label} did not settle: ${JSON.stringify(last)}`);
}

function command(args) {
  const result = spawnSync('docker', args, { encoding: 'utf8' });
  if (result.status !== 0) throw new Error(`docker ${args.join(' ')} failed: ${(result.stderr || result.stdout).trim()}`);
  return result.stdout.trim();
}

async function inventory(expectedOnline) {
  const result = await api('/api/admin/workers');
  return { ok: result.response.status === 200 && validWorkerInventory(result.body, workerId, expectedOnline), status: result.response.status };
}

function run(mode, extra = {}) {
  const status = runK6('control-plane-outage.js', {
    TARGET, WORKER_ID: workerId, ADMIN_TOKEN: adminToken, MODE: mode,
    RATE: process.env.RATE || 5, VUS: process.env.VUS || 10,
    DURATION: process.env.DURATION || '20s', SUMMARY_JSON: process.env.SUMMARY_JSON || '',
    PROXY_ENDPOINTS_FILE: proxyEndpointsFile,
    ...extra,
  });
  if (status !== 0) throw new Error(`control-plane ${mode} phase failed with exit ${status}`);
}

async function workerOutage() {
  if (!workerContainer) return false;
  let endpoints;
  try { endpoints = JSON.parse(readFileSync(proxyEndpointsFile, 'utf8')); } catch { endpoints = null; }
  if (!validHealthyProxyEndpoints(endpoints, workerId)) {
    throw new Error('worker outage requires HEALTHY_PROXY_ENDPOINTS_FILE with player and checker streams on other workers');
  }
  if (process.env.CONFIRM_WORKER_OUTAGE !== workerContainer) throw new Error('repeat OUTAGE_WORKER_CONTAINER in CONFIRM_WORKER_OUTAGE');
  const origin = new URL(TARGET).origin;
  if (!['127.0.0.1', 'localhost', '::1'].includes(new URL(TARGET).hostname) && process.env.CONFIRM_REMOTE_WORKER_OUTAGE !== origin) {
    throw new Error(`remote worker outage requires CONFIRM_REMOTE_WORKER_OUTAGE=${origin}`);
  }
  const running = command(['inspect', '--format', '{{.State.Running}}', workerContainer]);
  if (running !== 'true') throw new Error(`${workerContainer} is not running`);
  if (!(await inventory(true)).ok) throw new Error(`worker ${workerId} is not online before the outage drill`);
  let stopped = false;
  try {
    command(['stop', '--time', '5', workerContainer]);
    stopped = true;
    await waitFor(() => inventory(false), 'worker lease expiry');
    run('worker-offline');
  } finally {
    if (stopped) {
      command(['start', workerContainer]);
      await waitFor(() => inventory(true), 'worker reconnect');
    }
  }
  return true;
}

async function imageOutage() {
  const game = Number(process.env.IMAGE_OUTAGE_GAME || process.env.GAME);
  const challenge = Number(process.env.IMAGE_OUTAGE_CID);
  if (!Number.isSafeInteger(challenge) || challenge <= 0) return false;
  if (process.env.CONTROL_PLANE_IMAGE_OUTAGE_ACK !== '1') throw new Error('set CONTROL_PLANE_IMAGE_OUTAGE_ACK=1 for the disposable missing-image start');
  const origin = new URL(TARGET).origin;
  if (!['127.0.0.1', 'localhost', '::1'].includes(new URL(TARGET).hostname) && process.env.CONFIRM_REMOTE_IMAGE_OUTAGE !== origin) {
    throw new Error(`remote missing-image drill requires CONFIRM_REMOTE_IMAGE_OUTAGE=${origin}`);
  }
  if (!Number.isSafeInteger(game) || game <= 0) throw new Error('IMAGE_OUTAGE_GAME (or GAME) is required');
  if (!(await inventory(true)).ok) throw new Error(`worker ${workerId} must be online for the missing-image phase`);
  const image = sql(`SELECT COALESCE(build_image_digest,'') FROM "GameChallenges" WHERE game_id=${game} AND id=${challenge}`);
  if (!new RegExp(`^worker://${workerId}/sha256:[a-f0-9]{64}$`, 'i').test(image)) {
    throw new Error('IMAGE_OUTAGE_CID must be prepared with a worker-local immutable image owned by OUTAGE_WORKER_ID');
  }
  const player = sql(
    `SELECT p.id::text || '|' || u.id::text || '|' || u.security_stamp FROM "Participations" p ` +
      `JOIN "UserParticipations" up ON up.participation_id=p.id JOIN "AspNetUsers" u ON u.id=up.user_id ` +
      `WHERE p.game_id=${game} AND p.status=1 AND u.role=1 AND u.email_confirmed ` +
      `AND NOT EXISTS(SELECT 1 FROM "GameInstances" i WHERE i.participation_id=p.id AND i.challenge_id=${challenge}) ` +
      `ORDER BY p.id,u.id LIMIT 1`,
  );
  if (!player) throw new Error('missing-image phase needs one accepted player without an existing instance');
  const [participationId, userId, stamp] = player.split('|');
  const operationId = randomUUID();
  const playerToken = mintJwt(userId, stamp, 1);
  const result = await api(`/api/game/${game}/container/${challenge}`, {
    method: 'POST', timeoutMs: 130_000,
    headers: { Authorization: `Bearer ${playerToken}`, 'X-RSCTF-Operation-ID': operationId },
  });
  if (!isBoundedImageFailure(result.response.status, result.text)) {
    throw new Error(`prepared missing-image start returned ${result.response.status}: ${result.text.slice(0, 300)}`);
  }
  const workload = sql(
    `SELECT observed_state || '|' || COALESCE(observed_message,'') FROM "WorkerWorkloads" ` +
      `WHERE owner_kind='container' AND owner_key='player-container:${operationId}'`,
  );
  if (!/^(Failed|Absent)\|/i.test(workload)) {
    throw new Error(`missing-image failure was not durably classified: ${workload || 'missing workload'}`);
  }
  run('image-unavailable', { GAME: game, CID: challenge });
  const published = sql(`SELECT COUNT(*) FROM "GameInstances" WHERE participation_id=${Number(participationId)} AND challenge_id=${challenge} AND container_id IS NOT NULL`);
  if (published !== '0') throw new Error('failed image workload was published as a player instance');
  return true;
}

const workerPhaseRan = await workerOutage();
const imagePhaseRan = await imageOutage();
if (!workerPhaseRan && !imagePhaseRan) {
  throw new Error('configure OUTAGE_WORKER_CONTAINER and/or IMAGE_OUTAGE_CID for at least one outage phase');
}
console.log('control_plane_outage_ok');
