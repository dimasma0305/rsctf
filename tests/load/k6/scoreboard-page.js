// Compare the retained full-board contract with the bounded page at held throughput.
import http from 'k6/http';
import { Rate, Trend } from 'k6/metrics';

const target = String(__ENV.TARGET || 'http://127.0.0.1:8080').replace(/\/+$/, '');
const game = Number(__ENV.GAME);
const teams = Number(__ENV.TEAMS || 500);
const rate = Number(__ENV.RATE || 10);
const vus = Number(__ENV.VUS || 8);
const duration = String(__ENV.DURATION || '30s');
const durationMatch = duration.match(/^([1-9]\d*)(s|m)$/);
const durationSeconds = durationMatch ? Number(durationMatch[1]) * (durationMatch[2] === 'm' ? 60 : 1) : 0;
const paged = __ENV.SCOREBOARD_CASE === 'page';
const conditional = __ENV.CONDITIONAL === '1';
const tokens = JSON.parse(open(__ENV.TOKENS_FILE));
if (!Number.isSafeInteger(game) || game < 1 || !['full', 'page'].includes(__ENV.SCOREBOARD_CASE)
  || !Number.isInteger(rate) || rate < 1 || rate > 2000 || !Number.isInteger(vus) || vus < 1 || vus > 500
  || durationSeconds < 1 || durationSeconds > 600 || !Array.isArray(tokens) || tokens.length < 100 || tokens.length > 4000) {
  throw new Error('bounded GAME, RATE, VUS, DURATION, 100+ tokens, and SCOREBOARD_CASE=full|page required');
}

const serverErrors = new Rate('server_5xx');
const violations = new Rate('scoreboard_page_violation');
const notModified = new Rate('scoreboard_304_ratio');
const encodedBytes = new Trend('scoreboard_encoded_bytes');
const decodedBytes = new Trend('scoreboard_decoded_bytes');
const parseTime = new Trend('scoreboard_parse_ms', true);
const fullResponseTime = new Trend('scoreboard_200_ms', true);
let validator;

export const options = {
  scenarios: { standings: { executor: 'constant-arrival-rate', rate, timeUnit: '1s', duration,
    preAllocatedVUs: vus, maxVUs: vus } },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    server_5xx: ['rate==0'], scoreboard_page_violation: ['rate==0'],
    dropped_iterations: ['count==0'], http_req_duration: ['p(95)<800'],
    ...(conditional ? { scoreboard_304_ratio: ['rate>0.7'] } : {}),
  },
};

export default function () {
  const token = tokens[(__ITER + (__VU - 1) * 17) % tokens.length];
  const path = `/api/game/${game}/scoreboard${paged ? '/page?skip=0&count=30' : ''}`;
  const headers = { Authorization: `Bearer ${token}`, 'Accept-Encoding': 'gzip' };
  if (conditional && validator) headers['If-None-Match'] = validator;
  const response = http.get(target + path, { headers, tags: { endpoint: paged ? 'page' : 'full' } });
  serverErrors.add(response.status >= 500);
  notModified.add(response.status === 304);
  const etag = response.headers.ETag || response.headers.Etag;
  let invalid = ![200, 304].includes(response.status)
    || !etag || !response.headers['X-Scoreboard-Version'];
  if (response.status === 200) {
    validator = etag;
    encodedBytes.add(Number(response.headers['Content-Length']));
    decodedBytes.add(response.body.length);
    fullResponseTime.add(response.timings.duration);
    const started = Date.now();
    try {
      const board = JSON.parse(response.body);
      parseTime.add(Date.now() - started);
      invalid ||= board.items.length !== (paged ? Math.min(30, teams) : teams)
        || new Set(board.items.map(row => row.id)).size !== board.items.length
        || (paged && (board.total !== teams || board.skip !== 0 || !board.snapshotVersion));
    } catch { invalid = true; }
  } else if (response.status === 304) {
    invalid ||= !conditional || (response.body || '').length !== 0;
  }
  violations.add(invalid);
}
