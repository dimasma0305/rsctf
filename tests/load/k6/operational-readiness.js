import http from 'k6/http';
import { Rate, Trend } from 'k6/metrics';

const failures = new Rate('readiness_failures');
const server5xx = new Rate('server_5xx');
const latency = new Trend('readiness_poll_ms', true);
export const options = {
  scenarios: { readiness: { executor: 'constant-arrival-rate', rate: 1, timeUnit: '1s', duration: '30s', preAllocatedVUs: 4, maxVUs: 8 } },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: { readiness_failures: ['rate==0'], server_5xx: ['rate==0'], dropped_iterations: ['count==0'], readiness_poll_ms: ['p(95)<200'] },
};

export default function () {
  const headers = { Authorization: `Bearer ${__ENV.ADMIN_TOKEN}` };
  for (const [path, verify] of [
    [`/api/edit/games/${__ENV.GAME}/ad/koth/state`, body => Array.isArray(body.excludedHills) && body.excludedHills.some(hill => hill.challengeId === 819 && hill.reasons.includes('TargetNotReady'))],
    ['/api/admin/repobindings', body => Array.isArray(body.data) && body.data.some(binding => binding.id === Number(__ENV.BINDING) && binding.scanOutcome === 'PolicyBlocked')],
  ]) {
    const response = http.get(__ENV.TARGET + path, { headers, timeout: '5s' });
    server5xx.add(response.status >= 500);
    latency.add(response.timings.duration);
    let valid = false;
    try {
      const body = response.json();
      valid = response.status === 200 && verify(path.endsWith('/state') ? (body.data ?? body) : body);
    } catch { /* Count malformed JSON as failure. */ }
    failures.add(!valid);
  }
  const health = http.get(__ENV.TARGET + '/healthz', { timeout: '5s' });
  server5xx.add(health.status >= 500);
  failures.add(health.status !== 200 || health.body !== 'ok');
}
