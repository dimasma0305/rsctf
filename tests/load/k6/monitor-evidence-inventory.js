// One fixed-rate request per iteration across every newly bounded monitor read.
import http from 'k6/http';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const GAME = __ENV.GAME || '';
const FIXTURE = JSON.parse(open(__ENV.FIXTURE_FILE || ''));
const RATE = Number(__ENV.RATE || 4);
const VUS = Number(__ENV.VUS || 16);
if (!/^\d+$/.test(GAME) || !Array.isArray(FIXTURE.tokens) || FIXTURE.tokens.length < 4) {
  throw new Error('GAME and a monitor evidence/inventory fixture with four tokens are required');
}

const invalidResponse = new Rate('monitor_inventory_invalid');
const oversizedBody = new Rate('monitor_inventory_oversized');
const server5xx = new Rate('monitor_inventory_5xx');
const rateLimited = new Rate('monitor_inventory_429');
const busyWithoutRetry = new Rate('monitor_inventory_busy_without_retry');
const monitorDuration = new Trend('monitor_inventory_ms', true);
const healthFailure = new Rate('monitor_inventory_health_failure');
const healthDuration = new Trend('monitor_inventory_health_ms', true);

export const options = {
  scenarios: {
    monitorReads: {
      executor: 'constant-arrival-rate',
      exec: 'monitorRead',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
    exactHealth: {
      executor: 'constant-arrival-rate',
      exec: 'health',
      rate: 1,
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: 1,
      maxVUs: 2,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    monitor_inventory_invalid: ['rate==0'],
    monitor_inventory_oversized: ['rate==0'],
    monitor_inventory_5xx: ['rate==0'],
    monitor_inventory_429: ['rate==0'],
    monitor_inventory_busy_without_retry: ['rate==0'],
    monitor_inventory_health_failure: ['rate==0'],
    dropped_iterations: ['count==0'],
    monitor_inventory_ms: ['p(95)<1000'],
    monitor_inventory_health_ms: ['p(95)<500'],
  },
};

let reportEtag = null;

const endpoints = [
  { kind: 'incident', path: `/api/game/${GAME}/cheatinfo/page?limit=100` },
  { kind: 'delta', path: `/api/game/${GAME}/cheatinfo/page?limit=100&afterId=0` },
  { kind: 'report', path: `/api/game/${GAME}/cheatreport` },
  { kind: 'evidence', path: `/api/game/${GAME}/cheatreport/events/${FIXTURE.eventId}` },
  {
    kind: 'compare',
    path: `/api/game/${GAME}/cheatreport/compare?participationA=${FIXTURE.pair[0]}&participationB=${FIXTURE.pair[1]}`,
  },
  { kind: 'challenge', path: `/api/game/games/${GAME}/captures/page?count=10000` },
  { kind: 'team', path: `/api/game/captures/${FIXTURE.challengeId}/page?count=10000` },
  {
    kind: 'file',
    path: `/api/game/captures/${FIXTURE.challengeId}/${FIXTURE.participationId}/page?count=10000`,
  },
];

function token(index) {
  return FIXTURE.tokens[index % FIXTURE.tokens.length];
}

function header(response, name) {
  const target = name.toLowerCase();
  for (const [key, value] of Object.entries(response.headers || {})) {
    if (key.toLowerCase() === target) return value;
  }
  return null;
}

function parse(response) {
  try {
    return response.json();
  } catch (_) {
    return null;
  }
}

function validPage(body, itemsKey = 'items') {
  const rows = body && Array.isArray(body[itemsKey]) ? body[itemsKey] : null;
  return rows !== null && rows.length <= 100 && (body.nextCursor === null || typeof body.nextCursor === 'string');
}

function validIncident(body, delta) {
  if (!body || !Array.isArray(body.data) || body.data.length > 100 || !Number.isSafeInteger(body.checkpointId)) return false;
  const ids = new Set();
  let previous = -1;
  for (const row of body.data) {
    if (!row || !Number.isSafeInteger(row.id) || !Number.isFinite(row.observedAt) || ids.has(row.id)) return false;
    if (delta && row.id <= previous) return false;
    previous = row.id;
    ids.add(row.id);
  }
  return typeof body.hasMore === 'boolean' &&
    (body.nextBefore === null ||
      (Number.isFinite(body.nextBefore.observedAt) && Number.isSafeInteger(body.nextBefore.id)));
}

function semantic(endpoint, response) {
  if (endpoint.kind === 'report' && response.status === 304) return String(response.body || '').length === 0;
  if (response.status !== 200) return false;
  const body = parse(response);
  if (endpoint.kind === 'incident' || endpoint.kind === 'delta') return validIncident(body, endpoint.kind === 'delta');
  if (endpoint.kind === 'report') {
    return body && Number.isFinite(body.generatedAt) && Array.isArray(body.suspicionList) && Array.isArray(body.collusionGroups);
  }
  if (endpoint.kind === 'evidence') return body && Number.isSafeInteger(body.eventId) && Array.isArray(body.sources);
  if (endpoint.kind === 'compare') return body && Number.isFinite(body.rsi) && Array.isArray(body.details) && body.details.length <= 50;
  return validPage(body);
}

export function monitorRead() {
  const sequence = exec.scenario.iterationInTest;
  const endpoint = endpoints[sequence % endpoints.length];
  const headers = {
    Authorization: `Bearer ${token(sequence)}`,
    'X-Real-IP': `31.${1 + (sequence % 240)}.${1 + (Math.floor(sequence / 240) % 250)}.${1 + (sequence % 250)}`,
  };
  if (endpoint.kind === 'report' && reportEtag) headers['If-None-Match'] = reportEtag;
  const response = http.get(`${TARGET}${endpoint.path}`, {
    headers,
    responseType: 'text',
    tags: { endpoint: endpoint.kind },
  });
  monitorDuration.add(response.timings.duration);
  server5xx.add(response.status >= 500);
  rateLimited.add(response.status === 429);
  busyWithoutRetry.add(response.status === 503 && !/^\d+$/.test(String(header(response, 'retry-after') || '')));

  if (endpoint.kind === 'report' && response.status === 200) reportEtag = header(response, 'etag');
  const byteLimit = endpoint.kind === 'report' ? 4 * 1024 * 1024 : 512 * 1024;
  oversizedBody.add(String(response.body || '').length > byteLimit);
  invalidResponse.add(!semantic(endpoint, response));
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, { responseType: 'text', tags: { endpoint: 'healthz' } });
  healthDuration.add(response.timings.duration);
  healthFailure.add(response.status !== 200 || response.body !== 'ok');
}
