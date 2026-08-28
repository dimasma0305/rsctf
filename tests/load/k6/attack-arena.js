// Fixed-rate model of already-open public attack arenas. One iteration is one
// completion-scheduled 15-second client cycle and performs exactly the four
// canonical reads used by Attack.tsx; a separate lane keeps health observable.
import http from 'k6/http';
import { Rate, Trend } from 'k6/metrics';

const TARGET = String(__ENV.TARGET || '').replace(/\/+$/, '');
const GAME = String(__ENV.GAME || '');
const RATE = Number(__ENV.RATE || 80);
const VUS = Number(__ENV.VUS || 120);
const MAX_VUS = Number(__ENV.MAX_VUS || 480);
const DURATION = __ENV.DURATION || '60s';
const durationMatch = DURATION.match(/^([1-9]\d*)(s|m)$/);
const durationSeconds = durationMatch ? Number(durationMatch[1]) * (durationMatch[2] === 'm' ? 60 : 1) : 0;

if (!TARGET || !/^\d+$/.test(GAME)) throw new Error('TARGET and positive GAME are required');
if (![RATE, VUS, MAX_VUS].every(Number.isSafeInteger) || RATE < 1 || RATE > 1000 || VUS < 1 || VUS > 500 || MAX_VUS < VUS || MAX_VUS > 2000) {
  throw new Error('RATE must be 1..1000, VUS 1..500, and MAX_VUS VUS..2000');
}
if (!Number.isSafeInteger(durationSeconds) || durationSeconds < 1 || durationSeconds > 600) {
  throw new Error('DURATION must be 1s..10m');
}

const routes = [
  `/api/Game/${GAME}/Ad/Scoreboard`,
  `/api/game/${GAME}/ad/koth/scoreboard`,
  `/api/game/${GAME}/scoreboard`,
  `/api/game/${GAME}`,
];
const invalid = new Rate('arena_invalid_response');
const server5xx = new Rate('server_5xx');
const notFound = new Rate('arena_404');
const rateLimited = new Rate('arena_429');
const cycleMs = new Trend('arena_cycle_ms', true);

export const options = {
  scenarios: {
    spectators: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: VUS,
      maxVUs: MAX_VUS,
    },
    health: {
      executor: 'constant-arrival-rate',
      exec: 'health',
      rate: 2,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: 2,
      maxVUs: 8,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    arena_invalid_response: ['rate==0'],
    server_5xx: ['rate==0'],
    arena_404: ['rate==0'],
    arena_429: ['rate<0.001'],
    dropped_iterations: ['count==0'],
    arena_cycle_ms: ['p(95)<1000'],
    http_req_duration: ['p(95)<800'],
  },
};

export default function () {
  const started = Date.now();
  const responses = http.batch(
    routes.map((path) => [
      'GET',
      `${TARGET}${path}`,
      null,
      { headers: { Accept: 'application/json' }, responseType: 'text', timeout: '10s', tags: { endpoint: path } },
    ]),
  );
  cycleMs.add(Date.now() - started);
  for (const response of responses) {
    server5xx.add(response.status >= 500);
    notFound.add(response.status === 404);
    rateLimited.add(response.status === 429);
    invalid.add(response.status !== 200);
  }
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, { responseType: 'text', timeout: '2s' });
  server5xx.add(response.status >= 500);
  invalid.add(response.status !== 200 || response.body !== 'ok');
}
