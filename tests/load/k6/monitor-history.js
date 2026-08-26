// Fixed-rate bounded-history regression. One iteration is exactly one request.
import http from 'k6/http';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const GAME = __ENV.GAME || '';
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ''));
const RATE = Number(__ENV.RATE || 1);
const VUS = Number(__ENV.VUS || 4);
if (!/^\d+$/.test(GAME) || TOKENS.length === 0) {
  throw new Error('GAME and at least one Monitor/Admin token are required');
}

const longSearch = 'needle'.repeat(100);
const endpoints = [
  { name: 'events_zero', path: `/api/game/${GAME}/events?count=0`, maxRows: 100 },
  { name: 'events_one', path: `/api/game/${GAME}/events?count=1`, maxRows: 1 },
  { name: 'events_max', path: `/api/game/${GAME}/events?count=100`, maxRows: 100 },
  { name: 'events_clamp', path: `/api/game/${GAME}/events?count=10000`, maxRows: 100 },
  { name: 'events_wildcard', path: `/api/game/${GAME}/events?count=100&search=${encodeURIComponent('%_')}`, maxRows: 100 },
  { name: 'events_long', path: `/api/game/${GAME}/events?count=100&search=${longSearch}`, maxRows: 100 },
  { name: 'submissions_zero', path: `/api/game/${GAME}/submissions?count=0`, maxRows: 100 },
  { name: 'submissions_one', path: `/api/game/${GAME}/submissions?count=1`, maxRows: 1 },
  { name: 'submissions_max', path: `/api/game/${GAME}/submissions?count=100`, maxRows: 100 },
  { name: 'submissions_clamp', path: `/api/game/${GAME}/submissions?count=10000`, maxRows: 100 },
  { name: 'submissions_wildcard', path: `/api/game/${GAME}/submissions?count=100&search=${encodeURIComponent('%_')}`, maxRows: 100 },
  { name: 'submissions_long', path: `/api/game/${GAME}/submissions?count=100&search=${longSearch}`, maxRows: 100 },
];

const invalidResponse = new Rate('monitor_history_invalid');
const rowLimitViolated = new Rate('monitor_history_row_limit_violated');
const oversizedBody = new Rate('monitor_history_oversized_body');
const server5xx = new Rate('server_5xx');
const rateLimited = new Rate('rate_limited');
const duration = new Trend('monitor_history_ms', true);

export const options = {
  scenarios: {
    boundedMonitorHistory: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '20s',
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    monitor_history_invalid: ['rate==0'],
    monitor_history_row_limit_violated: ['rate==0'],
    monitor_history_oversized_body: ['rate==0'],
    server_5xx: ['rate==0'],
    rate_limited: ['rate==0'],
    dropped_iterations: ['count==0'],
    monitor_history_ms: ['p(95)<800'],
  },
};

export default function () {
  const sequence = exec.scenario.iterationInTest;
  const endpoint = endpoints[sequence % endpoints.length];
  const token = TOKENS[sequence % TOKENS.length];
  const response = http.get(`${TARGET}${endpoint.path}`, {
    headers: { Authorization: `Bearer ${token}` },
    responseType: 'text',
    tags: { endpoint: endpoint.name },
  });
  duration.add(response.timings.duration);
  server5xx.add(response.status >= 500);
  rateLimited.add(response.status === 429);
  oversizedBody.add(String(response.body || '').length > 262144);

  let body = null;
  try {
    body = response.json();
  } catch (_) {
    // Reported by the semantic metric below.
  }
  const rows = body && Array.isArray(body.data) ? body.data : null;
  invalidResponse.add(response.status !== 200 || rows === null);
  rowLimitViolated.add(rows !== null && rows.length > endpoint.maxRows);
}
