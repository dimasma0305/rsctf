// Fixed-rate Redis outage micro-harness. The Node orchestrator stops only the
// explicitly acknowledged disposable Redis container and restores it in a
// finally block; this script measures the HTTP path while Redis is unavailable.
import http from 'k6/http';
import { check } from 'k6';
import { Rate } from 'k6/metrics';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const unexpectedStatus = new Rate('unexpected_status');

http.setResponseCallback(http.expectedStatuses(400));

export const options = {
  scenarios: {
    outage: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.RATE || 1),
      timeUnit: '1s',
      duration: __ENV.DURATION || '15s',
      preAllocatedVUs: Number(__ENV.VUS || 32),
      maxVUs: Number(__ENV.MAX_VUS || 64),
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    checks: ['rate==1'],
    http_req_failed: ['rate==0'],
    http_req_duration: ['p(95)<1000'],
    unexpected_status: ['rate==0'],
    dropped_iterations: ['count==0'],
  },
};

export default function () {
  const response = http.post(`${TARGET}/api/account/register`, '{', {
    headers: { 'Content-Type': 'application/json' },
    timeout: '20s',
  });
  const expected = response.status === 400;
  unexpectedStatus.add(!expected);
  check(response, { 'malformed request rejected': () => expected });
}
