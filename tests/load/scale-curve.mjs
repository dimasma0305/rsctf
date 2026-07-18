// BYOC scaling curve: bring up N idle tunnels at each step, measure rsctf's STEADY CPU
// as a median of many samples (robust to the occasional checker-tick / re-drive spike),
// and report the marginal CPU cost per tunnel so the curve shape is visible — linear,
// sub-linear, or super-linear. Answers "what happens past 120?" and "is 60→120 real?".
//   STEPS=30,60,120,200,300 SAMPLES=15 npm run scale
import { discover, stat, sleep } from './lib.mjs';
import * as byoc from './byoc-agents.mjs';

const STEPS = (process.env.STEPS || '30,60,120,200,300').split(',').map(Number);
const SAMPLES = Number(process.env.SAMPLES || 15);
const SETTLE_MS = Number(process.env.SETTLE_MS || 14000);

/** SAMPLES back-to-back rsctf CPU readings → {median, max, min, ram}. Each docker-stats
 *  call samples ~1s internally, so this spans ~SAMPLES seconds of wall clock. */
function sampleCpu() {
  const cpu = [];
  let ram = 0;
  for (let i = 0; i < SAMPLES; i++) {
    const s = stat();
    cpu.push(s.cpu);
    ram = Math.max(ram, parseFloat(s.mem)); // rsctf stays in the MiB range
  }
  cpu.sort((a, b) => a - b);
  return { median: cpu[cpu.length >> 1], max: cpu[cpu.length - 1], min: cpu[0], ram };
}

async function main() {
  const d = discover();
  console.log(`scale curve: byocChal=${d.byocChal} | steps=[${STEPS}] | ${SAMPLES} samples/step`);
  const allCapabilities = byoc.capabilitiesFor(Math.max(...STEPS), d.byocChal);
  byoc.teardown(); // clean any leftovers
  const base = sampleCpu();
  console.log(`  baseline (0 tunnels): median ${base.median}%  max ${base.max}%  RAM ${base.ram}MiB\n`);

  const rows = [];
  for (const N of STEPS) {
    byoc.teardown();
    const capabilities = allCapabilities.slice(0, N);
    const svc = byoc.startSharedService();
    process.stdout.write(`  N=${String(N).padEnd(4)} spawning…`);
    const t0 = Date.now();
    await byoc.startAgents(capabilities, svc);
    const waited = await byoc.waitTunnels(N, 150);
    const spawnS = ((Date.now() - t0) / 1000) | 0;
    await sleep(SETTLE_MS);
    const s = sampleCpu();
    const up = byoc.tunnelsUp();
    rows.push({ N, up, ...s });
    console.log(` up ${up}/${N} in ${spawnS}s (wait ${waited}s) | idle median ${s.median}% max ${s.max}% | RAM ${s.ram}MiB`);
  }
  byoc.teardown();

  console.log('\n  === scaling curve (idle, steady) ===');
  console.log('  tunnels │ median CPU │  max CPU │ Δmedian vs base │ CPU per tunnel │ RAM MiB');
  for (const r of rows) {
    const d0 = r.median - base.median;
    const per = (d0 / r.N).toFixed(3);
    console.log(
      `  ${String(r.N).padStart(6)}  │ ${String(r.median + '%').padStart(9)}  │ ${String(r.max + '%').padStart(7)}  │ ${String('+' + d0.toFixed(1) + '%').padStart(14)}  │ ${String(per + '%').padStart(13)}  │ ${String(r.ram).padStart(6)}`
    );
  }
  console.log('\n  A flat "CPU per tunnel" column = linear scaling; rising = super-linear; falling = sub-linear.');
}

main()
  .catch((e) => {
    console.error('error:', e.message);
    process.exitCode = 1;
  })
  .finally(() => {
    try {
      byoc.teardown();
    } catch {}
  });
