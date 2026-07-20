// BYOC request flood — hammer the tunnels' service listeners (the ip:port each BYOC
// tunnel exposes on rsctf). Requests route rsctf → yamux 'S' stream → the team's agent
// → its service, so this stresses the tunnel multiplexing, not an /api rate-limited
// route. Driven by scenarios/byoc.mjs, which discovers the listeners into LISTENERS.
//
//   LISTENERS=ip:port,ip:port,... VUS=250 DURATION=30s k6 run byoc-requests.js
import http from 'k6/http';
import { Trend, Rate } from 'k6/metrics';

const LISTENERS = (__ENV.LISTENERS || '').split(',').filter(Boolean);
if (!LISTENERS.length) throw new Error('LISTENERS must contain the exact registered BYOC fleet');
const REPORTABLE = __ENV.RSCTF_ACCEPTANCE_REPORTABLE === '1';
const lat = new Trend('byoc_req_ms', true);
const server5xx = new Rate('server_5xx');
const nonok = new Rate('non_200');

export const options = {
  scenarios: {
    flood: {
      executor: 'constant-vus',
      vus: Number(__ENV.VUS || 250),
      duration: __ENV.DURATION || '30s',
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: REPORTABLE
    ? {
        server_5xx: ['rate==0'],
        non_200: ['rate==0'],
        http_req_failed: ['rate==0'],
      }
    : { server_5xx: ['rate<0.01'] },
  // NOKEEPALIVE=1 → a fresh TCP per request, so each attack opens a new yamux 'S'
  // stream through the tunnel (stream-churn stress), not one stream reused by keep-alive.
  noConnectionReuse: __ENV.NOKEEPALIVE === '1',
};

export default function () {
  if (!LISTENERS.length) return;
  const target = LISTENERS[(__VU * 7 + __ITER) % LISTENERS.length];
  const r = http.get(`http://${target}/`, {
    headers: { 'X-Real-IP': `11.${__VU % 254}.${__ITER % 254}.1` },
    timeout: '5s',
  });
  lat.add(r.timings.duration);
  server5xx.add(r.status >= 500);
  nonok.add(r.status !== 200);
}
