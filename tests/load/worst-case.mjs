// Worst-case: a mass BYOC reconnect storm. Bring up N tunnels, then RESTART rsctf so
// every agent's WebSocket drops at the same instant — the nastiest realistic event
// (an operator redeploy/crash mid-game). Assert that client jitter spreads successful
// reconnects while rsctf stays responsive (/livez) and dependency-ready (/healthz).
//   `N=120 npm run worst-case`
import { execFileSync } from 'node:child_process';
import { assertByocRestartTarget } from './byoc-harness.js';
import { discover, stat, docker, RSCTF, sleep } from './lib.mjs';
import { countContainerFatalLogs } from './log-audit.mjs';
import * as byoc from './byoc-agents.mjs';

const N = Number(process.env.N || 120);
const HOSTPORT = process.env.HOSTPORT || '127.0.0.1:8080';
const fmt = (s) => `${s.cpu}% CPU / ${s.mem} RAM`;

function checkedDocker(args, label) {
  const result = docker(args);
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = String(result.stderr || result.stdout || '').trim();
    throw new Error(`${label}: ${detail || `docker exited with status ${result.status}`}`);
  }
  return result;
}

function restartTargetIdentity() {
  const result = checkedDocker(['inspect', RSCTF], `inspect restart target ${RSCTF}`);
  const records = JSON.parse(result.stdout);
  if (records.length !== 1) throw new Error(`Docker inspection for ${RSCTF} was ambiguous`);
  return assertByocRestartTarget(process.env, records[0]);
}

function verifyAgentReconnects(sinceMs, expected) {
  const names = byoc.agentNames();
  if (names.length !== expected) {
    throw new Error(`reconnect evidence expected ${expected} relay containers, found ${names.length}`);
  }
  const since = new Date(sinceMs).toISOString();
  const connectedAt = [];
  for (const name of names) {
    const inspected = checkedDocker(['inspect', name], `inspect reconnected BYOC relay ${name}`);
    const [resource] = JSON.parse(inspected.stdout);
    if (!resource?.State?.Running) throw new Error(`BYOC relay ${name} stopped during reconnect`);
    const logs = checkedDocker(
      ['logs', '--timestamps', '--since', since, name],
      `read reconnect evidence from BYOC relay ${name}`,
    );
    const lines = `${logs.stdout}\n${logs.stderr}`
      .split('\n')
      .filter((line) => /tunnel connected/i.test(line));
    if (lines.length === 0) {
      throw new Error(`BYOC relay ${name} did not record a fresh tunnel connection after restart`);
    }
    const timestamp = Date.parse(lines.at(-1).trim().split(/\s+/, 1)[0]);
    if (!Number.isFinite(timestamp) || timestamp < sinceMs) {
      throw new Error(`BYOC relay ${name} emitted unusable reconnect timing evidence`);
    }
    connectedAt.push(timestamp);
  }
  const spreadMs = Math.max(...connectedAt) - Math.min(...connectedAt);
  const minimumSpreadMs = Number(process.env.MIN_RECONNECT_SPREAD_MS || 250);
  if (!Number.isSafeInteger(minimumSpreadMs) || minimumSpreadMs < 0 || minimumSpreadMs > 30_000) {
    throw new Error('MIN_RECONNECT_SPREAD_MS must be an integer between 0 and 30000');
  }
  if (expected >= 20 && spreadMs < minimumSpreadMs) {
    throw new Error(
      `BYOC reconnect cohort collapsed into ${spreadMs}ms (minimum ${minimumSpreadMs}ms)`,
    );
  }
  return { count: names.length, spreadMs };
}

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
  const runStartedAt = Date.now();
  const targetBefore = restartTargetIdentity();
  const d = discover();
  console.log(`worst-case reconnect storm: N=${N} tunnels | idle rsctf: ${fmt(stat())}`);

  const capabilities = byoc.capabilitiesFor(N, d.byocChal);
  const svc = byoc.startSharedService();
  await byoc.startAgents(capabilities, svc);
  const initialWait = await byoc.waitTunnels(N);
  await sleep(4000);
  console.log(`  exactly ${N} tunnels registered in ${initialWait}s; steady: ${fmt(stat())}`);

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
  console.log('  RESTARTING rsctf → all agent sockets drop on one synchronized clock ...');
  let reconnectError;
  let reconnectSeconds;
  try {
    checkedDocker(['restart', RSCTF], `restart disposable rsctf replica ${RSCTF}`);
    const targetAfter = restartTargetIdentity();
    if (targetAfter.id !== targetBefore.id || targetAfter.startedAt === targetBefore.startedAt) {
      throw new Error(`${RSCTF} did not preserve identity and advance StartedAt across restart`);
    }
    await byoc.waitTunnels(N, 60);
    const reconnectEvidence = verifyAgentReconnects(t0, N);
    reconnectSeconds = ((Date.now() - t0) / 1000) | 0;
    console.log(
      `  exactly ${reconnectEvidence.count} tunnels re-registered at T0+${reconnectSeconds}s ` +
        `across ${reconnectEvidence.spreadMs}ms`,
    );
  } catch (error) {
    reconnectError = error;
  }
  await monitor;
  if (reconnectError) throw reconnectError;

  probe.lat.sort((a, b) => a - b);
  const pc = (x) => (probe.lat.length ? probe.lat[(probe.lat.length * x) | 0] : 0);
  const panics = countContainerFatalLogs(RSCTF, runStartedAt);
  console.log(`\n  RESULT — did rsctf stay responsive through the storm?`);
  console.log(`    livez: ${probe.ok} ok / ${probe.fail} fail  |  latency ms med ${pc(0.5) | 0} p95 ${pc(0.95) | 0} max ${Math.max(0, ...probe.lat) | 0}`);
  console.log(`    healthz: ${readiness.ok} ready / ${readiness.fail} unavailable`);
  console.log(`    reconnect: ${N}/${N} exact in ${reconnectSeconds}s`);
  console.log(`    peak CPU during storm: ${cpuPeak}%  |  panic/fatal lines: ${panics}`);
  console.log(`    (a handful of fails are the ~1-2s restart downtime itself, not the reconnect load)`);
  if (panics !== 0) throw new Error(`${RSCTF} emitted ${panics} panic/fatal log line(s)`);
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
