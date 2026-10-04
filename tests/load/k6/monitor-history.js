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
  { name: 'events_zero', path: `/api/game/${GAME}/events/page?count=0`, kind: 'history', feed: 'events', maxRows: 100 },
  { name: 'events_one', path: `/api/game/${GAME}/events/page?count=1`, kind: 'history', feed: 'events', maxRows: 1 },
  { name: 'events_max', path: `/api/game/${GAME}/events/page?count=100`, kind: 'history', feed: 'events', maxRows: 100 },
  { name: 'events_clamp', path: `/api/game/${GAME}/events/page?count=10000`, kind: 'history', feed: 'events', maxRows: 100 },
  { name: 'events_wildcard', path: `/api/game/${GAME}/events/page?count=100&search=${encodeURIComponent('%_')}`, kind: 'history', feed: 'events', maxRows: 100 },
  { name: 'events_long', path: `/api/game/${GAME}/events/page?count=100&search=${longSearch}`, kind: 'history', feed: 'events', maxRows: 100 },
  { name: 'event_checkpoint', path: `/api/game/${GAME}/events/backfill`, kind: 'checkpoint', feed: 'events', maxRows: 0 },
  { name: 'event_backfill_one', path: `/api/game/${GAME}/events/backfill?after=0&limit=1`, kind: 'backfill', feed: 'events', after: 0, maxRows: 1 },
  { name: 'event_backfill_max', path: `/api/game/${GAME}/events/backfill?after=0&limit=100`, kind: 'backfill', feed: 'events', after: 0, maxRows: 100 },
  { name: 'event_backfill_clamp', path: `/api/game/${GAME}/events/backfill?after=0&limit=10000`, kind: 'backfill', feed: 'events', after: 0, maxRows: 100 },
  { name: 'submissions_zero', path: `/api/game/${GAME}/submissions/page?count=0`, kind: 'history', feed: 'submissions', maxRows: 100 },
  { name: 'submissions_one', path: `/api/game/${GAME}/submissions/page?count=1`, kind: 'history', feed: 'submissions', maxRows: 1 },
  { name: 'submissions_max', path: `/api/game/${GAME}/submissions/page?count=100`, kind: 'history', feed: 'submissions', maxRows: 100 },
  { name: 'submissions_clamp', path: `/api/game/${GAME}/submissions/page?count=10000`, kind: 'history', feed: 'submissions', maxRows: 100 },
  { name: 'submissions_wildcard', path: `/api/game/${GAME}/submissions/page?count=100&search=${encodeURIComponent('%_')}`, kind: 'history', feed: 'submissions', maxRows: 100 },
  { name: 'submissions_long', path: `/api/game/${GAME}/submissions/page?count=100&search=${longSearch}`, kind: 'history', feed: 'submissions', maxRows: 100 },
  { name: 'submission_checkpoint', path: `/api/game/${GAME}/submissions/backfill`, kind: 'checkpoint', feed: 'submissions', maxRows: 0 },
  { name: 'submission_backfill_one', path: `/api/game/${GAME}/submissions/backfill?after=0&limit=1`, kind: 'backfill', feed: 'submissions', after: 0, maxRows: 1 },
  { name: 'submission_backfill_max', path: `/api/game/${GAME}/submissions/backfill?after=0&limit=100`, kind: 'backfill', feed: 'submissions', after: 0, maxRows: 100 },
  { name: 'submission_backfill_clamp', path: `/api/game/${GAME}/submissions/backfill?after=0&limit=10000`, kind: 'backfill', feed: 'submissions', after: 0, maxRows: 100 },
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

function validFeedRows(rows, endpoint, requireAscendingCursor) {
  const ids = new Set();
  const cursors = new Set();
  let previousCursor = endpoint.after || 0;
  for (const row of rows) {
    const commonShape =
      row !== null &&
      typeof row === 'object' &&
      Number.isSafeInteger(row.id) &&
      Number.isSafeInteger(row.cursor) &&
      row.cursor > 0 &&
      Number.isFinite(row.time) &&
      !ids.has(row.id) &&
      !cursors.has(row.cursor);
    const feedShape = endpoint.feed === 'events'
      ? Array.isArray(row.values) && typeof row.type === 'string'
      : typeof row.answer === 'string' && typeof row.status === 'string';
    if (!commonShape || !feedShape || (requireAscendingCursor && row.cursor <= previousCursor)) return false;
    ids.add(row.id);
    cursors.add(row.cursor);
    previousCursor = row.cursor;
  }
  return true;
}

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
  let rows = null;
  let valid = response.status === 200;
  if (endpoint.kind === 'history') {
    rows = Array.isArray(body) ? body : null;
    valid = valid && rows !== null && validFeedRows(rows, endpoint, false);
  } else {
    rows = body && Array.isArray(body[endpoint.feed]) ? body[endpoint.feed] : null;
    valid =
      valid &&
      rows !== null &&
      Number.isSafeInteger(body.nextCursor) &&
      body.nextCursor >= 0 &&
      typeof body.hasMore === 'boolean' &&
      validFeedRows(rows, endpoint, true);
    if (valid && endpoint.kind === 'checkpoint') {
      valid = rows.length === 0 && body.hasMore === false;
    } else if (valid) {
      valid =
        body.nextCursor === (rows.length === 0 ? endpoint.after : rows[rows.length - 1].cursor);
    }
  }
  invalidResponse.add(!valid);
  rowLimitViolated.add(rows !== null && rows.length > endpoint.maxRows);
}
