// BYOC scale + request flood: bring up N tunnels, flood their service listeners, report
// rsctf resource use, then tear everything down.  `N=60 npm run byoc`
import { discover, stat, sleep, runK6 } from './lib.mjs';
import * as byoc from './byoc-agents.mjs';

const N = Number(process.env.N || 60);

async function main() {
  const d = discover();
  console.log(`byoc scale: N=${N} tunnels, byocChal=${d.byocChal} | idle rsctf: ${fmt(stat())}`);

  const capabilities = byoc.capabilitiesFor(N, d.byocChal);
  const svc = byoc.startSharedService();
  console.log(`bringing up ${N} relay agents → ${svc} ...`);
  await byoc.startAgents(capabilities, svc);
  const waited = await byoc.waitTunnels(N);
  await sleep(3000);
  console.log(`  ${byoc.tunnelsUp()}/${N} tunnels up (${waited < 0 ? 'timeout' : waited + 's'}); with ${N} idle tunnels: ${fmt(stat())}`);

  const L = byoc.listeners();
  if (!L.length) throw new Error('no tunnel listeners registered');
  console.log(`flooding ${L.length} listeners ...`);
  runK6('byoc-requests.js', {
    LISTENERS: L.join(','),
    VUS: process.env.VUS || 250,
    DURATION: process.env.DURATION || '30s',
  });
  console.log(`  rsctf after flood: ${fmt(stat())}`);
}

const fmt = (s) => `${s.cpu}% CPU / ${s.mem} RAM`;

main()
  .catch((e) => {
    console.error('error:', e.message);
    process.exitCode = 1;
  })
  .finally(() => {
    byoc.teardown();
    console.log('  torn down');
  });
