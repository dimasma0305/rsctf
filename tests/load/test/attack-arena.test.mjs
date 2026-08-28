import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/attack-arena.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../attack-arena.mjs', import.meta.url), 'utf8');

test('attack arena models one bounded canonical cycle at a fixed arrival rate', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /http\.batch/);
  assert.equal((scenario.match(/`\/api\//g) || []).length, 4);
  for (const route of [
    '/api/Game/${GAME}/Ad/Scoreboard',
    '/api/game/${GAME}/ad/koth/scoreboard',
    '/api/game/${GAME}/scoreboard',
    '/api/game/${GAME}',
  ]) assert.ok(scenario.includes(route), `missing ${route}`);
  assert.doesNotMatch(scenario, /AttackFeed/);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  assert.match(scenario, /timeout: '10s'/);
  assert.match(scenario, /server_5xx: \['rate==0'\]/);
  assert.match(scenario, /arena_404: \['rate==0'\]/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(scenario, /durationSeconds > 600/);
  assert.match(scenario, /summaryTrendStats: \['avg', 'med', 'p\(90\)', 'p\(95\)', 'p\(99\)', 'max'\]/);
});

test('attack arena runner is explicit, read-only, and checks lifecycle after load', () => {
  assert.match(runner, /ATTACK_ARENA_LOAD_ACK/);
  assert.match(runner, /ALLOW_REMOTE_ATTACK_ARENA_LOAD/);
  assert.match(runner, /livePublic\(\) !== '1'/g);
  assert.match(runner, /vpn_access_required=FALSE/);
  assert.match(runner, /runK6\('attack-arena\.js'/);
  assert.doesNotMatch(runner, /\b(?:INSERT|UPDATE|DELETE)\b/);
});
