import http from 'k6/http';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

const TARGET = __ENV.TARGET;
const FIXTURE = JSON.parse(open(__ENV.FIXTURE_FILE || ''));
const RATE = Number(__ENV.RATE || 8);
const VUS = Number(__ENV.VUS || 24);
if (!TARGET || !FIXTURE.trusted?.publicKey || !FIXTURE.trusted?.teamToken || !FIXTURE.attacker?.publicKey) {
  throw new Error('public security fixture is incomplete');
}

const unexpected = new Rate('public_security_unexpected');
const unexpected5xx = new Rate('unexpected_server_5xx');
const healthFailure = new Rate('health_failure');
const cryptoLatency = new Trend('public_security_ms', true);
const healthLatency = new Trend('health_ms', true);

export const options = {
  scenarios: {
    crypto: {
      executor: 'constant-arrival-rate', rate: RATE, timeUnit: '1s',
      duration: __ENV.DURATION || '30s', preAllocatedVUs: VUS, maxVUs: VUS * 2,
    },
    health: {
      executor: 'constant-arrival-rate', exec: 'health', rate: 1, timeUnit: '1s',
      duration: __ENV.DURATION || '30s', preAllocatedVUs: 2, maxVUs: 4,
    },
  },
  thresholds: {
    public_security_unexpected: ['rate==0'], unexpected_server_5xx: ['rate==0'],
    health_failure: ['rate==0'], dropped_iterations: ['count==0'],
    public_security_ms: ['p(95)<1000'], health_ms: ['p(95)<1000'],
  },
};

function source(sequence) {
  return `39.${1 + (sequence % 200)}.${1 + (Math.floor(sequence / 200) % 200)}.${1 + (sequence % 250)}`;
}

export default function () {
  const sequence = exec.scenario.iterationInTest;
  const lane = sequence % 5;
  let response;
  if (lane === 0) {
    response = http.get(`${TARGET}/api/captcha/powchallenge`, {
      headers: { 'X-Real-IP': source(sequence) }, responseType: 'text', tags: { lane: 'pow' },
    });
    let validBody = true;
    if (response.status === 200) {
      try {
        const model = response.json();
        validBody = typeof model.id === 'string' && typeof model.challenge === 'string' &&
          Number.isInteger(model.difficulty) && model.expiresAt > Date.now() &&
          /(?:^|,)\s*no-store(?:\s*(?:,|$))/i.test(
            response.headers['Cache-Control'] || response.headers['cache-control'] || ''
          );
      } catch (_) { validBody = false; }
    }
    const limited = response.status === 429 && Boolean(response.headers['Retry-After'] || response.headers['retry-after']);
    unexpected.add(!((response.status === 200 && validBody) || limited));
  } else {
    const body = lane === 1 ? FIXTURE.trusted : lane === 2 ? FIXTURE.attacker : lane === 3
      ? { publicKey: 'bad', teamToken: 'bad' }
      : { publicKey: 'A'.repeat(1024), teamToken: FIXTURE.trusted.teamToken };
    response = http.post(`${TARGET}/api/team/verify`, JSON.stringify(body), {
      headers: { 'Content-Type': 'application/json', 'X-Real-IP': source(sequence) },
      responseType: 'text',
      tags: { lane: lane === 1 ? 'trusted' : lane === 2 ? 'attacker' : lane === 3 ? 'malformed' : 'oversized' },
    });
    const expected = lane === 1 ? 200 : lane === 2 ? 401 : lane === 3 ? 400 : 413;
    const limited = response.status === 429 && Boolean(response.headers['Retry-After'] || response.headers['retry-after']);
    unexpected.add(!(response.status === expected || limited));
  }
  cryptoLatency.add(response.timings.duration);
  unexpected5xx.add(response.status >= 500);
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, { responseType: 'text' });
  healthLatency.add(response.timings.duration);
  healthFailure.add(response.status !== 200 || response.body !== 'ok');
  unexpected5xx.add(false);
}
