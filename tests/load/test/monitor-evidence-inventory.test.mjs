import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/monitor-evidence-inventory.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../monitor-evidence-inventory.mjs', import.meta.url), 'utf8');
const routes = readFileSync(new URL('../../../src/controllers/game/routes.rs', import.meta.url), 'utf8');

test('traffic and anti-cheat monitor reads share a fixed-rate bounded contract gate', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /exec\.scenario\.iterationInTest/);
  assert.match(scenario, /cheatinfo\/page\?limit=100/);
  assert.match(scenario, /afterId=0/);
  assert.match(scenario, /cheatreport\/events/);
  assert.match(scenario, /cheatreport\/compare/);
  assert.match(scenario, /captures\/page\?count=10000/);
  assert.match(scenario, /rows\.length <= 100/);
  assert.match(scenario, /body\.data\.length > 100/);
  assert.match(scenario, /If-None-Match/);
  assert.match(scenario, /response\.status === 304/);
  assert.match(scenario, /response\.status === 503/);
  assert.match(scenario, /retry-after/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(scenario, /monitor_inventory_health_ms: \['p\(95\)<500'\]/);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
});

test('runner requires a large real inventory and protects minted monitor credentials', () => {
  for (const threshold of ['5_000', '1_000', '500', '20']) assert.ok(runner.includes(threshold));
  assert.match(runner, /"TrafficCaptureFiles"/);
  assert.match(runner, /"TrafficCaptureBuckets"/);
  assert.match(runner, /MONITOR_EVIDENCE_CAPTURE_ROOT/);
  assert.match(runner, /'-type', 'f', '-iname', '\*\.pcap'/);
  assert.match(runner, /filesystemFiles < minimums\.files/);
  assert.match(runner, /"CheatInfo"/);
  assert.match(runner, /"SuspicionEvents"/);
  assert.match(runner, /WHERE role IN \(2,3\)/);
  assert.match(runner, /mode: 0o600/);
  assert.match(runner, /rmSync\(fixtureDirectory, \{ recursive: true, force: true \}\)/);
  assert.match(runner, /body !== 'ok'/);
  assert.match(runner, /docker',\s*\['stats'/);
  assert.match(runner, /docker',\s*\['top'/);
  assert.match(runner, /pg_stat_database/);
  assert.match(runner, /MAX_MEMORY_DELTA_MIB/);
  assert.match(runner, /MAX_TASK_DELTA/);
  assert.match(runner, /MAX_BLOCK_IO_DELTA_MIB/);
  assert.match(runner, /MAX_PG_BLOCK_READ_DELTA/);
  assert.match(runner, /MAX_PG_TEMP_DELTA_MIB/);
  assert.match(runner, /\.resources\.json/);
  assert.match(runner, /ALLOW_REMOTE_MONITOR_EVIDENCE_STRESS/);
  assert.match(runner, /DURATION must be between 1s and 10m/);
});

test('every inventory and evidence route uses named query admission', () => {
  for (const handler of [
    'cheat_info_page',
    'cheat_report',
    'suspicion_event_evidence',
    'cheat_report_compare',
    'game_captures_page',
    'team_traffic_page',
    'traffic_files_page',
  ]) {
    assert.match(routes, new RegExp(`limited\\(Policy::Query, get\\(${handler}\\)\\)`));
  }
});
