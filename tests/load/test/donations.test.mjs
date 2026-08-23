import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/donations.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../donations.mjs', import.meta.url), 'utf8');

test('donation load is fixed-rate, read-only, bounded, and privacy-aware', () => {
  assert.match(scenario, /executor:\s*'constant-arrival-rate'/);
  assert.equal((scenario.match(/http\.get\(/g) || []).length, 1);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  assert.match(scenario, /\/api\/donations/);
  assert.match(scenario, /leaderboard\.length > 10/);
  assert.match(scenario, /messages\.length > 20/);
  assert.match(scenario, /supporteremail/);
  assert.match(scenario, /dropped_iterations:\s*\['count==0'\]/);
  assert.match(scenario, /server_5xx:\s*\['rate==0'\]/);
});

test('donation runner requires an explicit acknowledgement and exact health', () => {
  assert.match(runner, /DONATIONS_STRESS_ACK/);
  assert.match(runner, /RATE must be an integer between 1 and 500/);
  assert.equal((runner.match(/\/healthz/g) || []).length, 2);
  assert.doesNotMatch(runner, /\b(?:INSERT|UPDATE|DELETE)\b/);
});

test('public donation runner does not require a JWT minting secret', () => {
  const moduleUrl = new URL('../lib.mjs', import.meta.url).href;
  const probe = spawnSync(
    process.execPath,
    [
      '--input-type=module',
      '-e',
      `delete process.env.RSCTF_JWT_SECRET; const lib = await import(${JSON.stringify(moduleUrl)}); try { lib.mintJwt('00000000-0000-0000-0000-000000000000', 'stamp'); } catch (error) { if (/required for load-test token minting/.test(String(error))) process.exit(0); } process.exit(1);`,
    ],
    { encoding: 'utf8' },
  );
  assert.equal(probe.status, 0, probe.stderr || probe.stdout);
});
