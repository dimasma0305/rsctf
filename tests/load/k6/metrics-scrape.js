import http from 'k6/http';
import { Rate } from 'k6/metrics';

const target = String(__ENV.TARGET).replace(/\/+$/, '');
const token = __ENV.RSCTF_METRICS_TOKEN;
const rate = Number(__ENV.RATE || 100);
if (!/^http:\/\/(127\.0\.0\.1|localhost):\d+$/.test(target) || !token || token.length < 32
  || !Number.isSafeInteger(rate) || rate < 1 || rate > 1000) {
  throw new Error('loopback TARGET, strong RSCTF_METRICS_TOKEN, and RATE=1..1000 required');
}
const violations = new Rate('metrics_violation');
const serverErrors = new Rate('server_5xx');
const kinds = ['AdReset', 'AdReconcile', 'ChallengeBuild', 'BuildBatch', 'WorkloadRollout',
  'ImagePreflight', 'VariantGeneration', 'SecurityDerivation'];
export const options = {
  scenarios: {
    scrapes: { executor: 'constant-arrival-rate', rate, timeUnit: '1s', duration: '30s',
      preAllocatedVUs: 8, maxVUs: 16, exec: 'scrape' },
    health: { executor: 'constant-arrival-rate', rate: 10, timeUnit: '1s', duration: '30s',
      preAllocatedVUs: 2, maxVUs: 4, exec: 'health' },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: { server_5xx: ['rate==0'], metrics_violation: ['rate==0'],
    dropped_iterations: ['count==0'], http_req_failed: ['rate==0'],
    'http_req_duration{endpoint:metrics}': ['p(95)<250'],
    'http_req_duration{endpoint:health}': ['p(95)<1000'] },
};
export function scrape() {
  const response = http.get(`${target}/metrics`, {
    headers: { Authorization: `Bearer ${token}` }, tags: { endpoint: 'metrics' }, timeout: '3s',
  });
  serverErrors.add(response.status >= 500);
  const body = response.body || '';
  const series = body.split('\n').filter(line => line && !line.startsWith('#'));
  let invalid = response.status !== 200 || body.length >= 20000 || series.length >= 200
    || response.headers['Cache-Control'] !== 'no-store'
    || !response.headers['Content-Type']?.startsWith('text/plain; version=0.0.4')
    || body.includes(token);
  const names = new Set();
  for (const line of series) {
    const split = line.lastIndexOf(' ');
    const name = line.slice(0, split);
    invalid ||= names.has(name) || !Number.isFinite(Number(line.slice(split + 1)));
    names.add(name);
  }
  for (const kind of kinds) invalid ||= !names.has(`rsctf_control_job_reclaimed_total{kind="${kind}"}`);
  violations.add(invalid);
}
export function health() {
  const response = http.get(`${target}/healthz`, { tags: { endpoint: 'health' }, timeout: '3s' });
  serverErrors.add(response.status >= 500);
  violations.add(response.status !== 200 || response.body !== 'ok');
}
