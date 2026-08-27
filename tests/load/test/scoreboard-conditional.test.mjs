import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/scoreboard-conditional.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../scoreboard-conditional.mjs', import.meta.url), 'utf8');
const standardRoute = readFileSync(new URL('../../../src/controllers/game/scoreboard.rs', import.meta.url), 'utf8');
const kothRoute = readFileSync(new URL('../../../src/controllers/game/koth/scoreboard.rs', import.meta.url), 'utf8');

test('conditional scoreboard load is fixed-rate, read-only, compressed, and validator-aware', () => {
  assert.match(scenario, /executor:\s*'constant-arrival-rate'/);
  assert.equal((scenario.match(/http\.get\(/g) || []).length, 1);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  assert.match(scenario, /\/api\/game\/\$\{STANDARD_GAME\}\/scoreboard/);
  assert.match(scenario, /\/api\/game\/\$\{KOTH_GAME\}\/ad\/koth\/scoreboard/);
  assert.match(scenario, /'If-None-Match'/);
  assert.match(scenario, /'Accept-Encoding':\s*'br, gzip;q=0\.8'/);
  assert.match(scenario, /response\.status === 304/);
  assert.match(scenario, /scoreboard_encoded_bytes/);
  assert.match(scenario, /scoreboard_missing_encoded_length/);
  assert.match(scenario, /scoreboard_missing_version/);
  assert.match(scenario, /scoreboard_json_parse_ms/);
  assert.match(scenario, /scoreboard_304_ratio:\s*\['rate>0\.70'\]/);
  assert.match(scenario, /server_5xx:\s*\['rate==0'\]/);
  assert.match(scenario, /dropped_iterations:\s*\['count==0'\]/);
  assert.match(scenario, /RATE > 2000/);
  assert.match(scenario, /VUS > 500/);
  assert.match(scenario, /durationSeconds > 600/);
});

test('runner uses disposable credentials, samples CPU/RAM, and removes token artifacts', () => {
  assert.match(runner, /SELECT id::text \|\| '\|' \|\| security_stamp/);
  assert.doesNotMatch(runner, /\b(?:INSERT|UPDATE|DELETE)\b/);
  assert.match(runner, /writeFileSync\(tokenFile,\s*JSON\.stringify\(tokens\),\s*\{ mode: 0o600 \}\)/);
  assert.match(runner, /spawnSync\(\s*'docker',\s*\['stats',\s*'--no-stream'/);
  assert.match(runner, /cpuPercent/);
  assert.match(runner, /memoryMiB/);
  assert.match(runner, /resourceContainers\.length > 16/);
  assert.match(runner, /docker stats returned no samples/);
  assert.match(runner, /child\.kill\('SIGTERM'\)/);
  assert.match(runner, /rmSync\(tokenDirectory,\s*\{ recursive: true, force: true \}\)/);
});

test('conditional responses remain behind the existing authorization and lifecycle gates', () => {
  const standardHandler = standardRoute.slice(
    standardRoute.indexOf('pub async fn scoreboard('),
    standardRoute.indexOf('pub async fn challenge_solvers(')
  );
  const standardConditional = standardHandler.indexOf('scoreboard_encoding::scoped_response');
  assert.match(standardHandler, /headers:\s*HeaderMap/);
  assert.ok(standardHandler.indexOf('g.hidden && !is_monitor') < standardConditional);
  assert.ok(standardHandler.indexOf('Utc::now() < g.start_time_utc') < standardConditional);

  const kothHandler = kothRoute.slice(kothRoute.indexOf('pub async fn scoreboard('));
  const kothConditional = kothHandler.indexOf('scoreboard_encoding::scoped_response');
  assert.match(kothHandler, /headers:\s*HeaderMap/);
  assert.ok(kothHandler.indexOf('load_game_cached') < kothConditional);
  assert.ok(kothHandler.indexOf('can_view_koth_standings') < kothConditional);
});
