// Fixed-rate first-demand image build burst. The fixture is prepared and
// audited by image-storage.mjs; k6 owns only player starts and health traffic.
import http from 'k6/http';
import { check } from 'k6';
import exec from 'k6/execution';
import { Counter, Rate, Trend } from 'k6/metrics';

import { parseImageStorageContext, positiveInteger } from '../image-storage.js';

const TARGET = String(__ENV.TARGET || 'http://127.0.0.1:8080').replace(/\/$/, '');
const CONTEXT = parseImageStorageContext(__ENV.IMAGE_STORAGE_CONTEXT || '');
const START_TIMEOUT_SECONDS = positiveInteger(__ENV.START_TIMEOUT_SECONDS || 900, 'START_TIMEOUT_SECONDS', 1800);
const HEALTH_DURATION = __ENV.HEALTH_DURATION || '60s';
const HEALTH_RATE = positiveInteger(__ENV.HEALTH_RATE || 2, 'HEALTH_RATE', 20);

http.setResponseCallback(http.expectedStatuses(200));

const startAttempts = new Counter('image_start_attempts');
const startFailure = new Rate('image_start_failure');
const server5xx = new Rate('server_5xx');
const healthFailure = new Rate('health_failure');
const imageStartMs = new Trend('image_start_ms', true);
const healthMs = new Trend('health_ms', true);

export const options = {
  setupTimeout: '30s',
  scenarios: {
    first_start_burst: {
      executor: 'constant-arrival-rate',
      exec: 'firstStart',
      rate: CONTEXT.tokens.length,
      timeUnit: '1s',
      duration: '1s',
      // One spare VU absorbs k6's inclusive one-second boundary iteration;
      // firstStart caps actual HTTP work to the exact audited token count.
      preAllocatedVUs: CONTEXT.tokens.length + 1,
      maxVUs: CONTEXT.tokens.length + 1,
      gracefulStop: `${START_TIMEOUT_SECONDS}s`,
    },
    health_during_build: {
      executor: 'constant-arrival-rate',
      exec: 'health',
      rate: HEALTH_RATE,
      timeUnit: '1s',
      duration: HEALTH_DURATION,
      preAllocatedVUs: Math.max(2, HEALTH_RATE),
      maxVUs: Math.max(4, HEALTH_RATE * 2),
      gracefulStop: '5s',
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    checks: ['rate==1'],
    image_start_attempts: [`count==${CONTEXT.tokens.length}`],
    image_start_failure: ['rate==0'],
    server_5xx: ['rate==0'],
    health_failure: ['rate==0'],
    dropped_iterations: ['count==0'],
    health_ms: ['p(95)<1000'],
  },
};

export function firstStart() {
  const iteration = exec.scenario.iterationInTest;
  // constant-arrival-rate can schedule the inclusive boundary at exactly one
  // second. Keep the generated HTTP burst bound to the audited token set.
  if (iteration >= CONTEXT.tokens.length) return;
  const token = CONTEXT.tokens[iteration];
  startAttempts.add(1);
  const response = http.post(
    `${TARGET}/api/game/${CONTEXT.gameId}/container/${CONTEXT.challengeId}`,
    null,
    {
      headers: {
        Authorization: `Bearer ${token}`,
        'User-Agent': `rsctf-image-storage-stress/${iteration}`,
      },
      timeout: `${START_TIMEOUT_SECONDS}s`,
      tags: { operation: 'first_image_start' },
    },
  );
  const failed = response.status !== 200;
  startFailure.add(failed);
  server5xx.add(response.status >= 500);
  imageStartMs.add(response.timings.duration);
  check(response, { 'first-demand container start succeeds': () => !failed });
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, {
    timeout: '3s',
    tags: { operation: 'health_during_image_build' },
  });
  const failed = response.status !== 200 || response.body !== 'ok';
  healthFailure.add(failed);
  server5xx.add(response.status >= 500);
  healthMs.add(response.timings.duration);
  check(response, { 'health remains exact during image build': () => !failed });
}
