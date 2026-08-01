// Fixed-rate, read-only production smoke for RSCTF's dominant polling paths.
//
// The runner supplies a large disposable-user token cohort so the test measures
// application capacity instead of intentionally exhausting one account's query
// bucket. One iteration performs exactly one HTTP request, making RATE directly
// comparable to HTTP requests/second.
import http from 'k6/http';
import { Rate, Trend } from 'k6/metrics';
import { validCombinedBoard } from '../combined-scoreboard.js';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const JEO_GAME = __ENV.JEO_GAME || '';
const AD_GAME = __ENV.AD_GAME || '';
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ''));
const RATE = Number(__ENV.RATE || 300);
const VUS = Number(__ENV.VUS || 100);

if (!/^\d+$/.test(JEO_GAME) || !/^\d+$/.test(AD_GAME) || TOKENS.length < 100) {
  throw new Error('JEO_GAME, AD_GAME, and at least 100 disposable-user TOKENS are required');
}
if (!Number.isSafeInteger(RATE) || RATE <= 0 || !Number.isSafeInteger(VUS) || VUS <= 0) {
  throw new Error('RATE and VUS must be positive integers');
}

const endpoints = [
  { name: 'game_catalog', path: '/api/game', trend: new Trend('game_catalog_ms', true) },
  {
    name: 'jeo_scoreboard',
    path: `/api/game/${JEO_GAME}/scoreboard`,
    trend: new Trend('jeo_scoreboard_ms', true),
  },
  {
    name: 'ad_scoreboard',
    path: `/api/Game/${AD_GAME}/Ad/Scoreboard`,
    trend: new Trend('ad_scoreboard_ms', true),
  },
  {
    name: 'koth_scoreboard',
    path: `/api/game/${AD_GAME}/ad/koth/scoreboard`,
    trend: new Trend('koth_scoreboard_ms', true),
  },
  {
    name: 'combined_scoreboard',
    path: `/api/game/${AD_GAME}/scoreboard/combined`,
    trend: new Trend('combined_scoreboard_ms', true),
  },
  {
    name: 'koth_timeline',
    path: `/api/game/${AD_GAME}/ad/koth/timeline`,
    trend: new Trend('koth_timeline_ms', true),
  },
];

const non200 = new Rate('non_200');
const server5xx = new Rate('server_5xx');
const rateLimited = new Rate('rate_limited');
const authRejected = new Rate('auth_rejected');
const combinedBoardInvalid = new Rate('combined_board_invalid');
const combinedBoardUncompressed = new Rate('combined_board_uncompressed');

export const options = {
  discardResponseBodies: true,
  scenarios: {
    polledReads: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '60s',
      preAllocatedVUs: VUS,
      maxVUs: VUS * 4,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    non_200: ['rate==0'],
    rate_limited: ['rate==0'],
    auth_rejected: ['rate==0'],
    combined_board_invalid: ['rate==0'],
    combined_board_uncompressed: ['rate==0'],
    server_5xx: ['rate==0'],
    dropped_iterations: ['count==0'],
    http_req_duration: ['p(95)<800'],
  },
};

function sourceIp(index) {
  return `31.${1 + (index % 240)}.${1 + (Math.floor(index / 240) % 250)}.${1 + (index % 250)}`;
}

export default function () {
  const sequence = (__VU - 1) * 997 + __ITER;
  const endpoint = endpoints[sequence % endpoints.length];
  // Do not correlate a token cohort with one endpoint. In particular, binding
  // the same fifth of the cohort to `/api/game` eventually measures its
  // deliberate heavy-query quota instead of the application read path.
  const tokenIndex = (sequence + Math.floor(sequence / endpoints.length)) % TOKENS.length;
  const headers = {
    Authorization: `Bearer ${TOKENS[tokenIndex]}`,
    'X-Real-IP': sourceIp(tokenIndex),
  };
  // k6 does not advertise a content coding by default. Browsers do, so make
  // the acceptance request explicit and fail if the cached gzip body regresses.
  if (endpoint.name === 'combined_scoreboard') headers['Accept-Encoding'] = 'gzip';
  const response = http.get(`${TARGET}${endpoint.path}`, {
    headers,
    responseType: endpoint.name === 'combined_scoreboard' ? 'text' : 'none',
    tags: { endpoint: endpoint.name },
  });
  endpoint.trend.add(response.timings.duration);
  non200.add(response.status !== 200);
  server5xx.add(response.status >= 500);
  rateLimited.add(response.status === 429);
  authRejected.add(response.status === 401 || response.status === 403);
  if (endpoint.name === 'combined_scoreboard') {
    let model = null;
    try {
      model = response.json();
    } catch (_) {
      // Invalid JSON is reported by the semantic metric below.
    }
    combinedBoardInvalid.add(response.status !== 200 || !validCombinedBoard(model));
    const encoding = String(response.headers['Content-Encoding'] || '').toLowerCase();
    combinedBoardUncompressed.add(response.status === 200 && encoding !== 'gzip' && encoding !== 'br');
  }
}
