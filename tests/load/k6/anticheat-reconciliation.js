// Fixed-rate public scoreboard and exact-health probes while anti-cheat is idle.
import http from 'k6/http';
import { Rate, Trend } from 'k6/metrics';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const GAME = __ENV.GAME || '';
const RATE = Number(__ENV.RATE || 20);
const VUS = Number(__ENV.VUS || 32);
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ''));

if (!/^\d+$/.test(GAME)) throw new Error('GAME is required');
if (
  !Array.isArray(TOKENS) ||
  TOKENS.length !== 2 ||
  !TOKENS.every((token) => typeof token === 'string' && token.length >= 32 && token.length <= 4_096)
) {
  throw new Error('exactly two bounded Admin tokens are required');
}
if (!Number.isSafeInteger(RATE) || RATE < 1 || RATE > 1_000) throw new Error('RATE must be in 1..1000');
if (!Number.isSafeInteger(VUS) || VUS < 2 || VUS > 1_024) throw new Error('VUS must be in 2..1024');

const scoreboardFailure = new Rate('anticheat_scoreboard_failure');
const scoreboardDuration = new Trend('anticheat_scoreboard_ms', true);
const healthFailure = new Rate('anticheat_health_failure');
const healthDuration = new Trend('anticheat_health_ms', true);

export const options = {
  scenarios: {
    scoreboardReads: {
      executor: 'constant-arrival-rate',
      exec: 'scoreboard',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '65s',
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
    exactHealth: {
      executor: 'constant-arrival-rate',
      exec: 'health',
      rate: 2,
      timeUnit: '1s',
      duration: __ENV.DURATION || '65s',
      preAllocatedVUs: 2,
      maxVUs: 4,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    anticheat_scoreboard_failure: ['rate==0'],
    anticheat_health_failure: ['rate==0'],
    dropped_iterations: ['count==0'],
    anticheat_scoreboard_ms: ['p(95)<750'],
    anticheat_health_ms: ['p(95)<500'],
  },
};

export function scoreboard() {
  const token = TOKENS[(__VU + __ITER) % TOKENS.length];
  const response = http.get(`${TARGET}/api/game/${GAME}/scoreboard`, {
    headers: { Authorization: `Bearer ${token}` },
    responseType: 'text',
    tags: { endpoint: 'scoreboard' },
  });
  scoreboardDuration.add(response.timings.duration);
  let body = null;
  try {
    body = response.json();
  } catch (_) {
    // Recorded as a semantic failure below.
  }
  scoreboardFailure.add(
    response.status !== 200 ||
      !body ||
      !Array.isArray(body.items) ||
      !Array.isArray(body.timelines) ||
      !Array.isArray(body.divisions) ||
      !Number.isSafeInteger(body.challengeCount) ||
      body.challengeCount < 0,
  );
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, {
    responseType: 'text',
    tags: { endpoint: 'healthz' },
  });
  healthDuration.add(response.timings.duration);
  healthFailure.add(response.status !== 200 || response.body !== 'ok');
}
