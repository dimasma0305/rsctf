// Worst-case: a mass BYOC reconnect storm. Bring up N tunnels, then RESTART rsctf so
// every agent's WebSocket drops and they all reconnect on their 3s backoff at once —
// the nastiest realistic event (an operator redeploy/crash mid-game). Measures whether
// rsctf stays responsive (/livez) and dependency-ready (/healthz) through the storm.
//   `N=120 npm run worst-case`
import { execFileSync } from 'node:child_process';
import { discover, stat, docker, sleep } from './lib.mjs';
import * as byoc from './byoc-agents.mjs';

const N = Number(process.env.N || 120);
const HOSTPORT = process.env.HOSTPORT || '127.0.0.1:8080';
const fmt = (s) => `${s.cpu}% CPU / ${s.mem} RAM`;

function health(path) {
  try {
    const out = execFileSync('curl', ['-s', '-o', '/dev/null', '-m', '2', '-w', '%{http_code}:%{time_total}', `http://${HOSTPORT}${path}`], { encoding: 'utf8' });
    const [code, t] = out.split(':');
    return { ok: code === '200', ms: parseFloat(t) * 1000 };
  } catch {
    return { ok: false, ms: 0 };
  }
}

async function main() {
  const d = discover();
  console.log(`worst-case reconnect storm: N=${N} tunnels | idle rsctf: ${fmt(stat())}`);

  const capabilities = byoc.capabilitiesFor(N, d.byocChal);
  const svc = byoc.startSharedService();
  await byoc.startAgents(capabilities, svc);
  await byoc.waitTunnels(N);
  await sleep(4000);
  console.log(`  steady with ${N} tunnels: ${fmt(stat())}`);

  // Responsiveness probe + CPU peak sampler, running across the restart.
  const probe = { ok: 0, fail: 0, lat: [] };
  const readiness = { ok: 0, fail: 0 };
  let cpuPeak = 0;
  const monitor = (async () => {
    for (let i = 0; i < 90; i++) {
      const h = health('/livez');
      if (h.ok) { probe.ok++; probe.lat.push(h.ms); } else probe.fail++;
      const ready = health('/healthz');
      readiness[ready.ok ? 'ok' : 'fail']++;
      try { cpuPeak = Math.max(cpuPeak, stat().cpu); } catch {}
      await sleep(400);
    }
  })();

  await sleep(2000);
  const t0 = Date.now();
  console.log('  RESTARTING rsctf → all agents reconnect on 3s backoff, at once ...');
  docker(['restart', 'rsctf-rsctf-1']);
  for (let i = 0; i < 60; i++) {
    if (byoc.tunnelsUp() >= N) {
      console.log(`  all ${N} tunnels re-registered at T0+${((Date.now() - t0) / 1000) | 0}s`);
      break;
    }
    await sleep(1000);
  }
  await monitor;

  probe.lat.sort((a, b) => a - b);
  const pc = (x) => (probe.lat.length ? probe.lat[(probe.lat.length * x) | 0] : 0);
  let panics = 0;
  try {
    panics = (execFileSync('docker', ['logs', 'rsctf-rsctf-1'], { encoding: 'utf8' }).match(/panic|FATAL/gi) || []).length;
  } catch {}
  console.log(`\n  RESULT — did rsctf stay responsive through the storm?`);
  console.log(`    livez: ${probe.ok} ok / ${probe.fail} fail  |  latency ms med ${pc(0.5) | 0} p95 ${pc(0.95) | 0} max ${Math.max(0, ...probe.lat) | 0}`);
  console.log(`    healthz: ${readiness.ok} ready / ${readiness.fail} unavailable`);
  console.log(`    peak CPU during storm: ${cpuPeak}%  |  panics: ${panics}`);
  console.log(`    (a handful of fails are the ~1-2s restart downtime itself, not the reconnect load)`);
}

main()
  .catch((e) => {
    console.error('error:', e.message);
    process.exitCode = 1;
  })
  .finally(() => {
    byoc.teardown();
    console.log('  torn down');
  });
