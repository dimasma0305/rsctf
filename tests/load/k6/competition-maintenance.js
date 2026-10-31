import http from 'k6/http';
import { Rate, Trend } from 'k6/metrics';

const failures = new Rate('maintenance_failures');
const server5xx = new Rate('server_5xx');
const latency = new Trend('maintenance_poll_ms', true);
const rate = Number(__ENV.RATE || 2);
if (!Number.isInteger(rate) || rate < 1 || rate > 2) {
  throw new Error('RATE must be 1 or 2 to respect the single administrator request budget');
}
export const options = {
  scenarios: {
    maintenance: {
      executor: 'constant-arrival-rate',
      rate,
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: 4,
      maxVUs: 16,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    maintenance_failures: ['rate==0'],
    server_5xx: ['rate==0'],
    maintenance_poll_ms: ['p(95)<200'],
    dropped_iterations: ['count==0'],
  },
};

export default function () {
  const response = http.get(`${__ENV.TARGET}/api/edit/games/${__ENV.GAME}/ad/maintenance`, {
    headers: { Authorization: `Bearer ${__ENV.ADMIN_TOKEN}` },
    timeout: '5s',
  });
  server5xx.add(response.status >= 500);
  latency.add(response.timings.duration);
  let valid = false;
  try {
    const body = response.json();
    const window = body.data ?? body;
    valid = response.status === 200 && window.gameId === Number(__ENV.GAME)
      && ['Requested', 'Editing', 'Applying', 'Completed', 'Cancelled'].includes(window.state);
  } catch { /* Count malformed responses as failed polls. */ }
  failures.add(!valid);
  const health = http.get(`${__ENV.TARGET}/healthz`, { timeout: '5s' });
  server5xx.add(health.status >= 500);
  failures.add(health.status !== 200 || health.body !== 'ok');
}
