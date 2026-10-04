// Fixed-rate public scoreboard wire/cache validation. Each VU retains one ETag
// per endpoint so synchronized spectators exercise 304s instead of repeatedly
// parsing the same maximum-roster board.
import http from 'k6/http';
import { Counter, Rate, Trend } from 'k6/metrics';

const TARGET = (__ENV.TARGET || 'http://127.0.0.1:8080').replace(/\/+$/, '');
const STANDARD_GAME = __ENV.STANDARD_GAME || '';
const KOTH_GAME = __ENV.KOTH_GAME || '';
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ''));
const RATE = Number(__ENV.RATE || 200);
const VUS = Number(__ENV.VUS || 100);
const DURATION = __ENV.DURATION || '60s';
const durationMatch = DURATION.match(/^([1-9]\d*)(s|m)$/);
const durationSeconds = durationMatch ? Number(durationMatch[1]) * (durationMatch[2] === 'm' ? 60 : 1) : 0;

if (
  !/^\d+$/.test(STANDARD_GAME) ||
  !/^\d+$/.test(KOTH_GAME) ||
  !Array.isArray(TOKENS) ||
  TOKENS.length < 100 ||
  TOKENS.length > 4000 ||
  !TOKENS.every((token) => typeof token === 'string' && token.length >= 32 && token.length <= 4096)
) {
  throw new Error('STANDARD_GAME, KOTH_GAME, and 100..4000 bounded disposable-user TOKENS are required');
}
if (
  !Number.isSafeInteger(RATE) ||
  RATE <= 0 ||
  RATE > 2000 ||
  !Number.isSafeInteger(VUS) ||
  VUS <= 0 ||
  VUS > 500 ||
  !Number.isSafeInteger(durationSeconds) ||
  durationSeconds <= 0 ||
  durationSeconds > 600
) {
  throw new Error('RATE must be 1..2000, VUS 1..500, and DURATION 1s..10m');
}

const endpoints = [`/api/game/${STANDARD_GAME}/scoreboard`, `/api/game/${KOTH_GAME}/ad/koth/scoreboard`];
const validators = Object.create(null);
const versions = Object.create(null);
const server5xx = new Rate('server_5xx');
const unexpectedStatus = new Rate('scoreboard_unexpected_status');
const missingValidator = new Rate('scoreboard_missing_validator');
const missingVersion = new Rate('scoreboard_missing_version');
const missingEncodedLength = new Rate('scoreboard_missing_encoded_length');
const uncompressed = new Rate('scoreboard_uncompressed_200');
const empty304Violation = new Rate('scoreboard_nonempty_304');
const notModified = new Rate('scoreboard_304_ratio');
const fullResponses = new Counter('scoreboard_200_count');
const encodedBytes = new Trend('scoreboard_encoded_bytes', true);
const parseMilliseconds = new Trend('scoreboard_json_parse_ms', true);

export const options = {
  scenarios: {
    spectators: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: VUS,
      maxVUs: VUS * 4,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    server_5xx: ['rate==0'],
    scoreboard_unexpected_status: ['rate==0'],
    scoreboard_missing_validator: ['rate==0'],
    scoreboard_missing_version: ['rate==0'],
    scoreboard_missing_encoded_length: ['rate==0'],
    scoreboard_uncompressed_200: ['rate==0'],
    scoreboard_nonempty_304: ['rate==0'],
    scoreboard_304_ratio: ['rate>0.70'],
    dropped_iterations: ['count==0'],
    http_req_duration: ['p(95)<800'],
  },
};

export default function () {
  const sequence = (__VU - 1) * 997 + __ITER;
  const path = endpoints[sequence % endpoints.length];
  const headers = {
    Authorization: `Bearer ${TOKENS[sequence % TOKENS.length]}`,
    'Accept-Encoding': 'br, gzip;q=0.8',
  };
  if (validators[path]) headers['If-None-Match'] = validators[path];
  const response = http.get(`${TARGET}${path}`, {
    headers,
    responseType: 'text',
    tags: { endpoint: path.includes('/koth/') ? 'koth' : 'standard' },
  });

  const accepted = response.status === 200 || response.status === 304;
  unexpectedStatus.add(!accepted);
  server5xx.add(response.status >= 500);
  notModified.add(response.status === 304);
  const etag = String(response.headers.ETag || response.headers.Etag || '');
  const version = String(response.headers['X-Scoreboard-Version'] || '');
  missingValidator.add(accepted && !etag);
  missingVersion.add(accepted && !version);
  unexpectedStatus.add(Boolean(response.status === 304 && versions[path] && versions[path] !== version));
  if (etag) validators[path] = etag;
  if (version) versions[path] = version;

  if (response.status === 304) {
    empty304Violation.add(String(response.body || '').length !== 0);
    encodedBytes.add(0);
    return;
  }
  empty304Violation.add(false);
  if (response.status !== 200) return;
  fullResponses.add(1);
  const encoding = String(response.headers['Content-Encoding'] || '').toLowerCase();
  uncompressed.add(encoding !== 'br' && encoding !== 'gzip');
  const contentLength = Number(response.headers['Content-Length']);
  const hasEncodedLength = Number.isSafeInteger(contentLength) && contentLength >= 0;
  missingEncodedLength.add(!hasEncodedLength);
  encodedBytes.add(hasEncodedLength ? contentLength : 0);
  const parseStarted = Date.now();
  const model = response.json();
  parseMilliseconds.add(Date.now() - parseStarted);
  const validStandard = Array.isArray(model?.items) && Array.isArray(model?.timelines);
  const validKoth = Array.isArray(model?.teams) && Array.isArray(model?.hills);
  unexpectedStatus.add(!(validStandard || validKoth));
}
