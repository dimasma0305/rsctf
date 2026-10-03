// Public spectator reads only; no credentials, submissions or administrative calls.
import http from 'k6/http';
import { Rate } from 'k6/metrics';
const target = __ENV.TARGET;
const game = __ENV.GAME;
if (!/^https?:\/\/(127\.0\.0\.1(?::\d+)?|tcp\.1pc\.tf)$/.test(target || '') || !/^[1-9]\d*$/.test(game || ''))
  throw new Error('A canonical or loopback target and positive GAME are required');
const paths = ['/healthz', `/api/game/${game}/scoreboard`, `/api/game/${game}/ad/scoreboard`, `/api/game/${game}/ad/koth/scoreboard`];
const bad = new Rate('arena_invalid_response'), errors = new Rate('server_5xx');
export const options = {
  scenarios: { spectators: { executor: 'constant-arrival-rate', rate: 2, timeUnit: '1s', duration: '30s', preAllocatedVUs: 4, maxVUs: 4 } },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: { arena_invalid_response: ['rate==0'], server_5xx: ['rate==0'], dropped_iterations: ['count==0'], http_req_duration: ['p(95)<800'] },
};
export default function () {
  const path = paths[(__ITER + __VU - 1) % paths.length];
  const response = http.get(target + path, { timeout: '5s' });
  errors.add(response.status >= 500);
  let valid = response.status === 200;
  if (path === '/healthz') valid &&= response.body === 'ok';
  else {
    try { const body = response.json(); valid &&= Array.isArray(body.items || body.teams); }
    catch { valid = false; }
  }
  bad.add(!valid);
}
