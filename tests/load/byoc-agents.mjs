// BYOC tunnel fleet orchestration for the load tests. Spins up one real relay agent
// per live Accepted participation, all forwarding to one shared nginx.
import { spawn } from 'node:child_process';
import { sql, docker, rsctfIp, byocCapabilities, GAME, NET, sleep } from './lib.mjs';

const AGENT_IMAGE =
  process.env.RSCTF_BYOC_AGENT_IMAGE ?? 'dimasmaualana/rsctf-byoc-agent:latest';
let activeCapabilities = [];

/** Derive capability URLs from current Accepted participations; secrets stay in lib.mjs. */
export const capabilitiesFor = (n, cid) => byocCapabilities(n, cid, GAME);

const fleetPredicate = (capabilities = activeCapabilities) => {
  if (!capabilities.length) return 'false';
  return `(participation_id,challenge_id) IN (${capabilities.map(({ pid, cid }) => `(${pid},${cid})`).join(',')})`;
};

/** Start the shared target service; returns its "host:port" for the agents. */
export function startSharedService() {
  const result = docker(['run', '-d', '--rm', '--name', 'load_svc', '--network', NET, 'nginx:alpine']);
  if (result.status !== 0) throw new Error(`failed to start shared BYOC service: ${result.stderr.trim()}`);
  return 'load_svc:80';
}

/** Launch N relay agents (each auto-reconnects on a 3s backoff — the reconnect-storm knob). */
export async function startAgents(capabilities, svcAddr) {
  if (!Array.isArray(capabilities) || capabilities.length === 0) {
    throw new Error('startAgents requires at least one live BYOC capability');
  }
  activeCapabilities = capabilities.map((capability) => ({ ...capability }));
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
        'run', '-d', '--rm', '--name', `load_agent_${i}`, '--network', NET,
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
}

/** Registered tunnel count (service rows the tunnels have pointed at a live listener). */
export function tunnelsUp() {
  return Number(sql(`SELECT count(DISTINCT (participation_id,challenge_id)) FROM "AdTeamServices" ` +
    `WHERE game_id=${GAME} AND ${fleetPredicate()} AND port>0`));
}

/** Await N tunnels registered (returns seconds waited, or -1 on timeout). */
export async function waitTunnels(n, timeoutS = 40) {
  for (let t = 0; t < timeoutS; t++) {
    if (tunnelsUp() >= n) return t;
    await sleep(1000);
  }
  return -1;
}

/** "ip:port" for every selected live listener (reachable from the host via the bridge). */
export function listeners() {
  const ip = rsctfIp();
  return (sql(
    `SELECT string_agg(port::text, ',' ORDER BY participation_id) FROM "AdTeamServices" ` +
      `WHERE game_id=${GAME} AND ${fleetPredicate()} AND port>0`
  ) || '')
    .split(',')
    .filter(Boolean)
    .map((p) => `${ip}:${p}`);
}

/** Stop every agent and restore the selected real service rows to Offline. */
export function teardown() {
  const ids = docker(['ps', '-aq', '--filter', 'name=load_agent_']).stdout.trim().split('\n').filter(Boolean);
  if (ids.length) docker(['rm', '-f', ...ids]);
  docker(['rm', '-f', 'load_svc']);
  if (activeCapabilities.length) {
    sql(`UPDATE "AdTeamServices" SET host='',port=0,status=2 WHERE game_id=${GAME} AND ${fleetPredicate()}`);
  }
  activeCapabilities = [];
}
