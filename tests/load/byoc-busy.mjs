// Busy tunnels: bring up N tunnels and pin them under SUSTAINED attack traffic (attackers
// hammering victims' exposed BYOC services), sampling rsctf CPU/RAM DURING the flood — so
// we see the cost of active tunnels (5ms re-drive + per-request 'S' stream churn + window
// growth), not the idle floor. Reports idle→busy delta, throughput, latency, errors.
//   N=80 VUS=400 DURATION=60s npm run busy
//   NOKEEPALIVE=1 N=80 VUS=400 npm run busy   # fresh stream per request (churn stress)
import { spawn } from 'node:child_process';
import { discover, stat, sleep } from './lib.mjs';
import * as byoc from './byoc-agents.mjs';

const N = Number(process.env.N || 80);
const VUS = Number(process.env.VUS || 400);
const DURATION = process.env.DURATION || '60s';
const fmt = (s) => `${s.cpu}% CPU / ${s.mem} RAM`;

async function main() {
  const d = discover();
  const churn = process.env.NOKEEPALIVE === '1';
  console.log(`busy tunnels: N=${N}, VUS=${VUS}, dur=${DURATION}, ${churn ? 'fresh stream/request (churn)' : 'keep-alive'} | byocChal=${d.byocChal}`);

  const capabilities = byoc.capabilitiesFor(N, d.byocChal);
  const svc = byoc.startSharedService();
  await byoc.startAgents(capabilities, svc);
  await byoc.waitTunnels(N);
  await sleep(6000);
  const idle = stat();
  console.log(`  idle @ ${N} tunnels: ${fmt(idle)}`);

  const L = byoc.listeners();
  console.log(`  flooding ${L.length} listeners for ${DURATION}, sampling rsctf during…`);

  const k6 = spawn('k6', ['run', new URL('./k6/byoc-requests.js', import.meta.url).pathname], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, LISTENERS: L.join(','), VUS: String(VUS), DURATION, NOKEEPALIVE: churn ? '1' : '' },
  });
  let out = '';
  let stderr = '';
  k6.stdout.on('data', (b) => (out += b));
  k6.stderr.on('data', (b) => {
    stderr = `${stderr}${b}`.slice(-32 * 1024);
  });

  let done = false;
  let k6Error;
  let k6ExitCode;
  const completion = new Promise((resolve) => {
    k6.on('error', (error) => {
      k6Error = error;
      done = true;
      resolve();
    });
    k6.on('close', (code) => {
      k6ExitCode = code;
      done = true;
      resolve();
    });
  });
  const cpu = [];
  let ramMax = 0;
  await sleep(3000); // let the flood ramp before sampling
  while (!done) {
    try {
      const s = stat();
      cpu.push(s.cpu);
      ramMax = Math.max(ramMax, parseFloat(s.mem));
    } catch {}
    await sleep(400); // yield so k6's 'close' event can fire (stat() is synchronous/blocking)
  }
  await completion;
  if (k6Error) throw k6Error;
  if (k6ExitCode !== 0) {
    throw new Error(
      `BYOC busy load failed with k6 status ${k6ExitCode ?? 'unknown'}${stderr.trim() ? `: ${stderr.trim().slice(-500)}` : ''}`,
    );
  }

  cpu.sort((a, b) => a - b);
  const med = cpu.length ? cpu[cpu.length >> 1] : 0;
  const max = cpu.length ? cpu[cpu.length - 1] : 0;
  const pick = (re) => (out.match(re) || [, '?'])[1];
  const rps = pick(/http_reqs[^\n]*?\s([\d.]+)\/s/);
  const lat = pick(/byoc_req_ms[^\n]*?p\(95\)=([^\s]+)/);
  const p99 = pick(/byoc_req_ms[^\n]*?p\(99\)=([^\s]+)/);
  const e5xx = pick(/server_5xx[^\n]*?:\s([\d.]+%)/);
  const non200 = pick(/non_200[^\n]*?:\s([\d.]+%)/);

  console.log(`\n  === busy profile @ N=${N}, ${VUS} VUs (${churn ? 'churn' : 'keep-alive'}) ===`);
  console.log(`    rsctf CPU under load : median ${med}%  max ${max}%   (idle was ${idle.cpu}%)`);
  console.log(`    rsctf RAM peak       : ${ramMax} MiB   (idle ${idle.mem})`);
  console.log(`    throughput           : ${rps} req/s`);
  console.log(`    latency              : p95 ${lat}  p99 ${p99}`);
  console.log(`    errors               : server_5xx ${e5xx}  non-200 ${non200}`);
  console.log(`    per-tunnel busy cost : ${((med - idle.cpu) / N).toFixed(2)}% CPU/tunnel over idle`);
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
