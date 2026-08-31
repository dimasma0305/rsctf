import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const runner = readFileSync(new URL('../managed-koth.mjs', import.meta.url), 'utf8');
const scenario = readFileSync(new URL('../k6/managed-koth.js', import.meta.url), 'utf8');
const fixture = readFileSync(new URL('../fixtures.mjs', import.meta.url), 'utf8');
const model = readFileSync(new URL('../managed-koth-model.js', import.meta.url), 'utf8');

test('managed KotH runner provisions hidden and paused before any live reporter cycle', () => {
  const provision = runner.slice(
    runner.indexOf('async function provision('),
    runner.indexOf('async function main()'),
  );
  const pause = provision.indexOf('A.setAdScoringPaused(current.gameId, true)');
  const ensure = provision.indexOf('/ad/EnsureContainers');
  const bootstrap = provision.indexOf('assertReporterFreeBootstrapTarget()');
  const schedule = provision.indexOf('A.setGameSchedule(');
  const resume = provision.indexOf('A.setAdScoringPaused(current.gameId, false)');
  assert.match(provision, /hidden: true/);
  assert.ok(pause > 0 && pause < ensure);
  assert.ok(ensure < bootstrap && bootstrap < schedule && schedule < resume);
  assert.match(runner, /managedKothHarnessConfig\(process\.env\)/);
  assert.match(model, /MANAGED_KOTH_STRESS_ACK/);
  assert.match(model, /MANAGED_KOTH_DISPOSABLE/);
  assert.match(runner, /ADMIN_LIFECYCLE_STACK_MARKER/);
});

test('managed KotH bootstrap waits for the committed target process to listen', () => {
  const bootstrap = runner.slice(
    runner.indexOf('async function assertReporterFreeBootstrapTarget()'),
    runner.indexOf('async function reporterStatus('),
  );
  assert.match(
    bootstrap,
    /waitUntil\([\s\S]*exactHealth\(arenaUrl, 'pre-cycle managed target'\)[\s\S]*\/reporter-status[\s\S]*60/,
  );
  assert.match(bootstrap, /reporterConfigured === false/);
  assert.match(bootstrap, /reporterHealthy === true/);
});

test('managed KotH runner uses only the injected reporter and keeps credentials ephemeral', () => {
  assert.match(runner, /validateManagedReporterEnvironment/);
  assert.match(runner, /signedOldReporterProbe\(target\.secret\)/);
  assert.match(runner, /response\.status === 401/);
  assert.match(runner, /writeFileSync\(tokenPath,[\s\S]*mode: 0o600/);
  assert.match(runner, /rmSync\(tokenSandbox, \{ recursive: true, force: true \}\)/);
  assert.doesNotMatch(runner, /kothApiObservation|kothApiCaptureWrite/);
  assert.doesNotMatch(runner, /HOME\s*:/);
});

test('managed KotH recovery reconstructs a dense prefix then resolves a new runtime', () => {
  assert.match(runner, /append-only reporter prefix reconstruction/);
  assert.match(runner, /reconstructed\.hash === restart\.before\.hash/);
  assert.match(runner, /A\.adScoringPaused\(current\.gameId\)/);
  assert.match(runner, /candidate\.containerId !== target\.containerId/);
  assert.match(runner, /arenaUrl: `http:\/\//);
  assert.match(runner, /snapshotRows/);
  assert.match(runner, /uniqueCrownRounds/);
  assert.match(runner, /crownMismatches/);
});

test('managed KotH traffic is fixed-arrival and gates auth abuse independently', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.doesNotMatch(scenario, /constant-vus/);
  assert.match(scenario, /valid_capabilities_exercised: \['count==2000'\]/);
  assert.match(scenario, /invalid_capabilities_rate_limited: \['count>0'\]/);
  assert.match(scenario, /invalid_retry_after: \['rate==0'\]/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(scenario, /server_5xx: \['rate==0'\]/);
});

test('managed target derives its bounded cohort and Crown from submitted scores', () => {
  assert.match(fixture, /scoreable = score > 0/);
  assert.match(fixture, /len\(active_hashes\) >= ACTIVE_FLEET/);
  assert.match(fixture, /len\(active_hashes\) == ACTIVE_FLEET/);
  assert.match(fixture, /sum\(1 for _, score in ranked if score > 0\) != ACTIVE_FLEET/);
  assert.match(fixture, /len\(leaders\) != 1/);
  assert.match(fixture, /selected = ordered/);
});
