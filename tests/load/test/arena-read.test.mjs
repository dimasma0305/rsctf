import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import test from 'node:test';
test('arena load is a bounded fixed-rate public read with health and roster integrity checks', () => {
  const scenario = readFileSync(new URL('../k6/arena-read.js',import.meta.url),'utf8');
  const runner = readFileSync(new URL('../arena-read.mjs',import.meta.url),'utf8');
  assert.match(scenario,/constant-arrival-rate/);
  assert.match(scenario,/rate: 2/); assert.match(scenario,/duration: '30s'/);
  assert.match(scenario,/response.body === 'ok'/);
  assert.match(scenario,/server_5xx: \['rate==0'\]/);
  assert.doesNotMatch(scenario,/http\.(post|put|patch|del|batch)\(/);
  assert.doesNotMatch(runner,/mintJwt|sql\(|fetch\([^\n]+method/);
  assert.match(runner,/assert.deepEqual\(after,before/);
  assert.match(runner,/new Set\(ids\).size/);
  for (const name of ['arena-read.mjs','k6/arena-read.js'])
    execFileSync(process.execPath,['--check',new URL(`../${name}`,import.meta.url).pathname]);
});
