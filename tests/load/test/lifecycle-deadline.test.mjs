import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const lifecycle = readFileSync(
  new URL('../lifecycle.mjs', import.meta.url),
  'utf8',
);

test('the event deadline outlives k6 in-flight iteration draining', () => {
  const defaultGrace = lifecycle.match(
    /process\.env\.EVENT_END_GRACE_SECONDS \|\| (\d+)/,
  );

  assert.ok(defaultGrace, 'EVENT_END_GRACE_SECONDS default is missing');
  assert.ok(
    Number(defaultGrace[1]) > 30,
    'event grace must exceed k6\'s 30-second graceful-stop window',
  );
  assert.match(
    lifecycle,
    /Math\.ceil\(runDuration\) \+ graceSeconds/,
    'the aligned deadline must include the drain grace',
  );
});

test('capacity VPN identities are provisioned before timed k6 traffic', () => {
  const setup = lifecycle.indexOf('capacity VPN identities ready:');
  const spawn = lifecycle.indexOf('const k6 = spawn("k6"');
  const timedLoop = lifecycle.indexOf('while (distributedTeamClients || !done)');

  assert.ok(setup >= 0, 'capacity VPN setup is missing');
  assert.ok(spawn > setup, 'capacity VPN setup must finish before k6 starts');
  assert.ok(timedLoop > spawn, 'timed lifecycle loop must follow k6 startup');
  assert.equal(
    lifecycle.match(/const capacityVpnPeers = new Set\(\);/g)?.length,
    1,
    'capacity VPN readiness must have one run-wide source of truth',
  );
});
