// Fixed-rate concurrent monitor XLSX exports plus an independent health probe.
import http from 'k6/http';
import { Counter, Rate, Trend } from 'k6/metrics';

import { classifyExportResponse } from '../monitor-export-model.js';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const GAME = Number(__ENV.GAME);
const TOKEN = __ENV.MONITOR_TOKEN || '';
const RATE = Number(__ENV.RATE || 2);
const VUS = Number(__ENV.VUS || 4);

if (!Number.isSafeInteger(GAME) || GAME <= 0 || !TOKEN) {
  throw new Error('GAME and MONITOR_TOKEN are required');
}
if (!Number.isSafeInteger(RATE) || RATE < 1 || RATE > 10 || !Number.isSafeInteger(VUS) || VUS < 2 || VUS > 32) {
  throw new Error('RATE must be 1..10 and VUS must be 2..32');
}

const invalidExport = new Rate('invalid_export_response');
const unexpected5xx = new Rate('unexpected_server_5xx');
const exportTimeout = new Rate('export_timeout');
const healthFailure = new Rate('health_failure');
const admitted = new Counter('exports_admitted');
const overloaded = new Counter('exports_overloaded');
const exportMs = new Trend('monitor_export_ms', true);
const healthMs = new Trend('monitor_export_health_ms', true);

export const options = {
  scenarios: {
    exports: {
      executor: 'constant-arrival-rate',
      exec: 'exportSheet',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: VUS,
      maxVUs: VUS,
    },
    health: {
      executor: 'constant-arrival-rate',
      exec: 'probeHealth',
      rate: 2,
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: 2,
      maxVUs: 4,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    invalid_export_response: ['rate==0'],
    unexpected_server_5xx: ['rate==0'],
    export_timeout: ['rate==0'],
    health_failure: ['rate==0'],
    exports_admitted: ['count>0'],
    monitor_export_health_ms: ['p(95)<500'],
    dropped_iterations: ['count==0'],
  },
};

const headers = { Authorization: `Bearer ${TOKEN}` };

export function exportSheet() {
  const kind = __ITER % 2 === 0 ? 'scoreboard' : 'submissions';
  const suffix = kind === 'scoreboard' ? 'scoreboardsheet' : 'submissionsheet';
  const response = http.get(`${TARGET}/api/game/${GAME}/${suffix}`, {
    headers,
    responseType: 'none',
    timeout: '25s',
    tags: { endpoint: `monitor_${kind}_export` },
  });
  exportMs.add(response.timings.duration, { kind });
  const result = classifyExportResponse(
    response.status,
    response.headers['Content-Type'],
    response.headers['Retry-After'],
  );
  invalidExport.add(!result.valid, { kind });
  unexpected5xx.add(response.status >= 500 && response.status !== 503, { kind });
  exportTimeout.add(response.status === 0, { kind });
  if (result.admitted) admitted.add(1, { kind });
  if (result.overloaded) overloaded.add(1, { kind });
}

export function probeHealth() {
  const response = http.get(`${TARGET}/healthz`, {
    responseType: 'text',
    timeout: '2s',
    tags: { endpoint: 'monitor_export_health' },
  });
  healthMs.add(response.timings.duration);
  healthFailure.add(response.status !== 200 || response.body !== 'ok');
}
