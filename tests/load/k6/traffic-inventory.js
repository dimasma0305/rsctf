import http from 'k6/http';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

import { validTrafficRows } from '../traffic-inventory.js';

const TARGET = __ENV.TARGET;
const GAME = __ENV.GAME;
const CID = __ENV.CID;
const PID = __ENV.PID;
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ''));
const RATE = Number(__ENV.RATE || 2);
const VUS = Number(__ENV.VUS || 8);
if (![GAME, CID, PID].every((value) => /^\d+$/.test(value)) || !TOKENS.length) throw new Error('GAME/CID/PID and monitor tokens are required');

const operations = [
  { kind: 'games', maxRows: 500, path: `/api/game/games/${GAME}/captures` },
  { kind: 'teams', maxRows: 100, path: `/api/game/captures/${CID}?count=100&skip=0` },
  { kind: 'teams', maxRows: 100, path: `/api/game/captures/${CID}?count=100&skip=100` },
  { kind: 'files', maxRows: 100, path: `/api/game/captures/${CID}/${PID}?count=100&skip=0` },
  { kind: 'files', maxRows: 100, path: `/api/game/captures/${CID}/${PID}?count=100&skip=100` },
];
const invalid = new Rate('traffic_inventory_invalid');
const server5xx = new Rate('server_5xx');
const rateLimited = new Rate('rate_limited');
const latency = new Trend('traffic_inventory_ms', true);

export const options = {
  scenarios: {
    inventory: { executor: 'constant-arrival-rate', rate: RATE, timeUnit: '1s', duration: __ENV.DURATION || '30s', preAllocatedVUs: VUS, maxVUs: VUS * 2 },
    health: { executor: 'constant-arrival-rate', exec: 'health', rate: 1, timeUnit: '1s', duration: __ENV.DURATION || '30s', preAllocatedVUs: 2, maxVUs: 4 },
  },
  thresholds: { traffic_inventory_invalid: ['rate==0'], server_5xx: ['rate==0'], rate_limited: ['rate==0'], dropped_iterations: ['count==0'], traffic_inventory_ms: ['p(95)<1000'] },
};

export default function () {
  const sequence = exec.scenario.iterationInTest;
  const operation = operations[sequence % operations.length];
  const response = http.get(`${TARGET}${operation.path}`, {
    headers: { Authorization: `Bearer ${TOKENS[sequence % TOKENS.length]}` }, responseType: 'text', tags: { endpoint: operation.kind },
  });
  latency.add(response.timings.duration);
  server5xx.add(response.status >= 500);
  rateLimited.add(response.status === 429);
  let body;
  try { body = response.json(); } catch (_) { body = null; }
  invalid.add(response.status !== 200 || !validTrafficRows(body, operation.kind, operation.maxRows, String(response.body || '').length));
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, { responseType: 'text' });
  server5xx.add(response.status >= 500);
  invalid.add(response.status !== 200 || response.body !== 'ok');
}
