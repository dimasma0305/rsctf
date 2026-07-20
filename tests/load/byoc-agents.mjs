// BYOC tunnel fleet orchestration for the load tests. Spins up one real relay agent
// per live Accepted participation, all forwarding to one shared nginx.
import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import {
  sql,
  docker,
  rsctfIp,
  byocCapabilities,
  DEFAULT_BYOC_AGENT_IMAGE,
  GAME,
  NET,
  sleep,
} from './lib.mjs';
import {
  assertByocFixtureImages,
  assertExactTunnelCount,
  assertOwnedByocContainer,
  byocFixtureLabelKeys,
  byocFixtureLabels,
  byocFixtureNames,
  dockerByocLabelArgs,
  dockerByocRunFilterArgs,
  normalizeByocRunId,
} from './byoc-harness.js';

export const BYOC_RUN_ID = normalizeByocRunId(
  process.env.RSCTF_BYOC_RUN_ID || `p${process.pid}-${randomBytes(4).toString('hex')}`,
);
const fixtureNames = byocFixtureNames(BYOC_RUN_ID);
const imageConfig = assertByocFixtureImages({
  agentImage: process.env.RSCTF_BYOC_AGENT_IMAGE ?? DEFAULT_BYOC_AGENT_IMAGE,
  serviceImage: process.env.RSCTF_BYOC_SERVICE_IMAGE ?? 'nginx:alpine',
  reportable: process.env.RSCTF_ACCEPTANCE_REPORTABLE === '1',
});
const AGENT_IMAGE = imageConfig.agentImage;
const SERVICE_IMAGE = imageConfig.serviceImage;
let activeCapabilities = [];

/** Derive capability URLs from current Accepted participations; secrets stay in lib.mjs. */
export const capabilitiesFor = (n, cid) => byocCapabilities(n, cid, GAME);

const fleetPredicate = (capabilities = activeCapabilities) => {
  if (!capabilities.length) return 'false';
  return `(participation_id,challenge_id) IN (${capabilities.map(({ pid, cid }) => `(${pid},${cid})`).join(',')})`;
};

function checkedDocker(args, label) {
  const result = docker(args);
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = String(result.stderr || result.stdout || '').trim();
    throw new Error(`${label}: ${detail || `docker exited with status ${result.status}`}`);
  }
  return result;
}

function validatedCapabilities(capabilities) {
  if (!Array.isArray(capabilities) || capabilities.length === 0) {
    throw new Error('startAgents requires at least one live BYOC capability');
  }
  const validated = capabilities.map((capability, index) => {
    const pid = Number(capability?.pid);
    const cid = Number(capability?.cid);
    const token = String(capability?.token || '');
    if (!Number.isSafeInteger(pid) || pid <= 0 || !Number.isSafeInteger(cid) || cid <= 0) {
      throw new Error(`BYOC capability ${index} has an invalid participation/challenge identity`);
    }
    if (!/^[0-9a-f]{64}$/.test(token)) {
      throw new Error(`BYOC capability ${index} has an invalid agent token`);
    }
    return Object.freeze({ pid, cid, token });
  });
  const identities = new Set(validated.map(({ pid, cid }) => `${pid}:${cid}`));
  if (identities.size !== validated.length) {
    throw new Error('BYOC capabilities must contain distinct participation/challenge pairs');
  }
  return validated;
}

function ownedContainerRecords({ runningOnly = false } = {}) {
  const listed = checkedDocker(
    ['ps', runningOnly ? '-q' : '-aq', ...dockerByocRunFilterArgs(BYOC_RUN_ID)],
    'discover run-owned BYOC containers',
  );
  const ids = listed.stdout.trim().split('\n').filter(Boolean);
  if (!ids.length) return [];
  const inspected = checkedDocker(['inspect', ...ids], 'inspect run-owned BYOC containers');
  const records = JSON.parse(inspected.stdout);
  for (const record of records) assertOwnedByocContainer(record, BYOC_RUN_ID);
  return records;
}

/** Start the shared target service; returns its "host:port" for the agents. */
export function startSharedService() {
  checkedDocker(
    [
      'run',
      '-d',
      '--rm',
      '--name',
      fixtureNames.service,
      ...dockerByocLabelArgs(byocFixtureLabels(BYOC_RUN_ID, 'shared-service')),
      '--network',
      NET,
      SERVICE_IMAGE,
    ],
    'start shared BYOC service',
  );
  const services = ownedContainerRecords({ runningOnly: true }).filter(
    (record) => record.Config?.Labels?.[byocFixtureLabelKeys.role] === 'shared-service',
  );
  assertExactTunnelCount(1, services.length, 'running BYOC shared-service container count');
  return `${fixtureNames.service}:80`;
}

/** Launch N relay agents (each auto-reconnects on a 3s backoff — the reconnect-storm knob). */
export async function startAgents(capabilities, svcAddr) {
  activeCapabilities = validatedCapabilities(capabilities);
  sql(`UPDATE "AdTeamServices" SET host='',port=0,status=2 WHERE game_id=${GAME} AND ${fleetPredicate()}`);
  const ip = rsctfIp();
  const limit = Math.min(40, activeCapabilities.length);
  const queue = activeCapabilities.map((capability, i) => ({ capability, i }));
  const errors = [];

  async function worker() {
    while (queue.length > 0) {
      const item = queue.shift();
      if (!item) break;
      const { capability, i } = item;
      const url = `ws://${ip}:8080/api/Game/${GAME}/Ad/Byoc/Agent/${capability.pid}/${capability.cid}/${capability.token}`;

      const args = [
        'run', '-d', '--rm', '--name', fixtureNames.agent(i),
        ...dockerByocLabelArgs(
          byocFixtureLabels(BYOC_RUN_ID, 'relay', {
            index: i,
            participationId: capability.pid,
            challengeId: capability.cid,
          }),
        ),
        '--network', NET,
        '-e', 'RSCTF_BYOC_MODE=agent',
        '-e', `RSCTF_BYOC_TUNNEL_URL=${url}`,
        '-e', `RSCTF_BYOC_SERVICE=${svcAddr}`,
        AGENT_IMAGE,
      ];

      try {
        await new Promise((resolve, reject) => {
          const child = spawn('docker', args);
          let stderr = '';
          child.stderr.on('data', (chunk) => (stderr += chunk));
          child.on('close', (code) => {
            if (code === 0) resolve();
            else reject(new Error(`agent ${i} (participation ${capability.pid}): ${stderr.trim()}`));
          });
          child.on('error', reject);
        });
      } catch (error) {
        errors.push(error);
      }
    }
  }

  const workers = Array.from({ length: limit }, worker);
  await Promise.all(workers);
  if (errors.length) {
    throw new Error(`failed to spawn ${errors.length} BYOC agent(s): ${errors.slice(0, 3).map((e) => e.message).join('; ')}`);
  }

  const relays = ownedContainerRecords({ runningOnly: true }).filter(
    (record) => record.Config?.Labels?.[byocFixtureLabelKeys.role] === 'relay',
  );
  assertExactTunnelCount(activeCapabilities.length, relays.length, 'running BYOC relay container count');
}

/** Registered tunnel count (service rows the tunnels have pointed at a live listener). */
export function tunnelsUp() {
  return Number(sql(`SELECT count(DISTINCT (participation_id,challenge_id)) FROM "AdTeamServices" ` +
    `WHERE game_id=${GAME} AND ${fleetPredicate()} AND port>0`));
}

/** Await exactly N registered tunnels; partial or oversized fleets fail closed. */
export async function waitTunnels(n, timeoutS = 40) {
  assertExactTunnelCount(activeCapabilities.length, n, 'requested BYOC tunnel count');
  let observed = 0;
  for (let t = 0; t <= timeoutS; t++) {
    observed = tunnelsUp();
    if (observed === n) return t;
    if (observed > n) {
      throw new Error(`BYOC tunnel count exceeded the exact fleet: expected ${n}, observed ${observed}`);
    }
    if (t === timeoutS) break;
    await sleep(1000);
  }
  throw new Error(`timed out waiting for exactly ${n} BYOC tunnels; observed ${observed}`);
}

/** "ip:port" for every selected live listener (reachable from the host via the bridge). */
export function listeners() {
  const ip = rsctfIp();
  const endpoints = (sql(
    `SELECT string_agg(port::text, ',' ORDER BY participation_id) FROM "AdTeamServices" ` +
      `WHERE game_id=${GAME} AND ${fleetPredicate()} AND port>0`
  ) || '')
    .split(',')
    .filter(Boolean)
    .map((p) => `${ip}:${p}`);
  assertExactTunnelCount(activeCapabilities.length, endpoints.length, 'BYOC listener count');
  if (new Set(endpoints).size !== endpoints.length) {
    throw new Error('BYOC listeners must expose one distinct endpoint per selected tunnel');
  }
  return endpoints;
}

/** Exact run-owned relay names, stable for reconnect log evidence. */
export function agentNames() {
  return activeCapabilities.map((_, index) => fixtureNames.agent(index));
}

/** Stop every agent and restore the selected real service rows to Offline. */
export function teardown() {
  const cleanupErrors = [];
  try {
    const resources = ownedContainerRecords();
    if (resources.length) {
      checkedDocker(
        ['rm', '-f', ...resources.map((resource) => resource.Id)],
        'remove run-owned BYOC containers',
      );
      if (ownedContainerRecords().length !== 0) {
        throw new Error('run-owned BYOC containers remain after cleanup');
      }
    }
  } catch (error) {
    cleanupErrors.push(error);
  }
  try {
    if (activeCapabilities.length) {
      sql(`UPDATE "AdTeamServices" SET host='',port=0,status=2 WHERE game_id=${GAME} AND ${fleetPredicate()}`);
    }
  } catch (error) {
    cleanupErrors.push(error);
  } finally {
    activeCapabilities = [];
  }
  if (cleanupErrors.length) {
    throw new Error(
      `BYOC cleanup failed: ${cleanupErrors.map((error) => error.message).join('; ')}`,
    );
  }
}
