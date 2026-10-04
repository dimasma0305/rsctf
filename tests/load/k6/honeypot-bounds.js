// Fixed-arrival public-decoy flood with an independent exact-health probe.
import http from 'k6/http';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const RATE = Number(__ENV.RATE || 512);
const VUS = Number(__ENV.VUS || 64);
const SOURCE_COUNT = Number(__ENV.SOURCE_COUNT || 16);

const baits = [
  '/.env',
  '/.git/config',
  '/.git/HEAD',
  '/wp-login.php',
  '/phpmyadmin',
  '/server-status',
  '/actuator/env',
  '/_ignition/execute-solution',
  '/backup.zip',
  '/database.sql',
];

if (!Number.isSafeInteger(RATE) || RATE < 1 || RATE > 10_000) throw new Error('RATE must be in 1..10000');
if (!Number.isSafeInteger(VUS) || VUS < 1 || VUS > 2_048) throw new Error('VUS must be in 1..2048');
if (!Number.isSafeInteger(SOURCE_COUNT) || SOURCE_COUNT < 1 || SOURCE_COUNT > 254) {
  throw new Error('SOURCE_COUNT must be in 1..254');
}

const decoyFailure = new Rate('honeypot_decoy_failure');
const decoyDuration = new Trend('honeypot_decoy_ms', true);
const healthFailure = new Rate('honeypot_health_failure');
const healthDuration = new Trend('honeypot_health_ms', true);

export const options = {
  scenarios: {
    decoyFlood: {
      executor: 'constant-arrival-rate',
      exec: 'decoy',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '20s',
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
    exactHealth: {
      executor: 'constant-arrival-rate',
      exec: 'health',
      rate: 1,
      timeUnit: '1s',
      duration: __ENV.DURATION || '20s',
      preAllocatedVUs: 1,
      maxVUs: 2,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    honeypot_decoy_failure: ['rate==0'],
    honeypot_health_failure: ['rate==0'],
    dropped_iterations: ['count==0'],
    honeypot_decoy_ms: ['p(95)<500'],
    honeypot_health_ms: ['p(95)<500'],
  },
};

export function decoy() {
  const sequence = exec.scenario.iterationInTest;
  const source = sequence % SOURCE_COUNT;
  const response = http.request(
    sequence % 2 === 0 ? 'GET' : 'POST',
    `${TARGET}${baits[sequence % baits.length]}`,
    null,
    {
      headers: {
        'User-Agent': `rsctf-honeypot-fixed-rate/${'x'.repeat(300)}`,
        // This gate must traverse a proxy whose immediate address is listed in
        // RSCTF_TRUSTED_PROXY_CIDRS. The runner verifies that these identities
        // survive hashing; a direct/untrusted target therefore fails closed.
        'X-Forwarded-For': `198.51.100.${source + 1}`,
      },
      responseType: 'text',
      tags: { endpoint: 'honeypot' },
    },
  );
  decoyDuration.add(response.timings.duration);
  decoyFailure.add(response.status !== 404 || response.body !== 'Not Found');
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, {
    responseType: 'text',
    tags: { endpoint: 'healthz' },
  });
  healthDuration.add(response.timings.duration);
  healthFailure.add(response.status !== 200 || response.body !== 'ok');
}
