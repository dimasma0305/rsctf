import http from 'k6/http';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

const TARGET = __ENV.TARGET;
const GAME = __ENV.GAME;
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ''));
const RATE = Number(__ENV.RATE || 2);
const VUS = Number(__ENV.VUS || 8);
const OPERATION_PREFIX = __ENV.OPERATION_PREFIX || '';
if (!TARGET || !/^\d+$/.test(GAME) || !TOKENS.length || !Number.isInteger(RATE) || RATE < 1
    || !/^[0-9a-f]{8}$/.test(OPERATION_PREFIX)) {
  throw new Error('TARGET, GAME, admin tokens, operation prefix, and positive RATE are required');
}

const invalid = new Rate('anti_cheat_reconcile_invalid');
const server5xx = new Rate('server_5xx');
const latency = new Trend('anti_cheat_reconcile_ms', true);

export const options = {
  scenarios: {
    reconcile: {
      executor: 'constant-arrival-rate', rate: RATE, timeUnit: '1s',
      duration: __ENV.DURATION || '30s', preAllocatedVUs: VUS, maxVUs: VUS * 2,
    },
    health: {
      executor: 'constant-arrival-rate', exec: 'health', rate: 1, timeUnit: '1s',
      duration: __ENV.DURATION || '30s', preAllocatedVUs: 2, maxVUs: 4,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    anti_cheat_reconcile_invalid: ['rate==0'],
    server_5xx: ['rate==0'],
    dropped_iterations: ['count==0'],
    anti_cheat_reconcile_ms: ['p(95)<5000'],
  },
};

function operationId(sequence) {
  const tail = (Number(sequence) + 1).toString(16).padStart(12, '0').slice(-12);
  return `${OPERATION_PREFIX}-0000-4000-8000-${tail}`;
}

export default function () {
  const sequence = exec.scenario.iterationInTest;
  const response = http.post(
    `${TARGET}/api/admin/games/${GAME}/derive-findings`,
    JSON.stringify({ operationId: operationId(sequence) }),
    {
      headers: {
        Authorization: `Bearer ${TOKENS[sequence % TOKENS.length]}`,
        'Content-Type': 'application/json',
      },
      responseType: 'text',
      tags: { endpoint: 'anti_cheat_reconcile' },
    },
  );
  let body;
  try { body = response.json(); } catch (_) { body = null; }
  const result = body?.data?.data ?? body?.data ?? body;
  const valid = response.status === 200
    && result?.operationId === operationId(sequence)
    && Number.isSafeInteger(result?.generation)
    && ['Running', 'Completed'].includes(result?.status);
  latency.add(response.timings.duration);
  server5xx.add(response.status >= 500);
  invalid.add(!valid);
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, { responseType: 'text' });
  server5xx.add(response.status >= 500);
  invalid.add(response.status !== 200 || response.body !== 'ok');
}
