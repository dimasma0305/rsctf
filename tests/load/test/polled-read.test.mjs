import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/polled-read.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../polled-read.mjs', import.meta.url), 'utf8');

test('polled-read is fixed-rate, read-only, and covers every dominant board', () => {
  assert.match(scenario, /executor:\s*'constant-arrival-rate'/);
  assert.equal((scenario.match(/http\.get\(/g) || []).length, 1);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  for (const path of [
    '/api/game',
    '/scoreboard',
    '/Ad/Scoreboard',
    '/ad/koth/scoreboard',
    '/scoreboard/combined',
    '/ad/koth/timeline',
  ]) {
    assert.ok(scenario.includes(path), `missing polled path ${path}`);
  }
  assert.match(scenario, /dropped_iterations:\s*\['count==0'\]/);
  assert.match(scenario, /server_5xx:\s*\['rate==0'\]/);
  assert.match(scenario, /validCombinedBoard/);
  assert.match(scenario, /combined_board_invalid/);
  assert.match(scenario, /combined_board_uncompressed/);
  assert.match(scenario, /response\.headers\['Content-Encoding'\]/);
  assert.match(scenario, /responseType:\s*endpoint\.name === 'combined_scoreboard' \? 'text' : 'none'/);
});

test('polled-read protects disposable credentials and avoids endpoint-correlated buckets', () => {
  assert.match(runner, /SELECT id::text \|\| '\|' \|\| security_stamp/);
  assert.doesNotMatch(runner, /\b(?:INSERT|UPDATE|DELETE)\b/);
  assert.match(runner, /writeFileSync\(tokenFile,\s*JSON\.stringify\(tokens\),\s*\{\s*mode:\s*0o600\s*\}\)/);
  assert.match(runner, /rmSync\(tokenDirectory,\s*\{\s*recursive:\s*true,\s*force:\s*true\s*\}\)/);
  assert.match(scenario, /sequence \+ Math\.floor\(sequence \/ endpoints\.length\)/);
});
