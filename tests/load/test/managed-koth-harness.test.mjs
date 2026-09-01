import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const runner = readFileSync(new URL('../managed-koth.mjs', import.meta.url), 'utf8');
const scenario = readFileSync(new URL('../k6/managed-koth.js', import.meta.url), 'utf8');
const fixture = readFileSync(new URL('../fixtures.mjs', import.meta.url), 'utf8');
const model = readFileSync(new URL('../managed-koth-model.js', import.meta.url), 'utf8');
const workflow = readFileSync(new URL('../../../.github/workflows/managed-koth-load.yml', import.meta.url), 'utf8');

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

test('managed KotH recovery submits a dense wave then resolves a new runtime', () => {
  const restart = runner.slice(
    runner.indexOf('async function restartManagedReporterProcess('),
    runner.indexOf('function integritySnapshot()'),
  );
  assert.match(runner, /restarted reporter exact dense wave/);
  const reconstruction = runner.slice(
    runner.indexOf("'pre-recovery dense score rows'"),
    runner.indexOf('const revoked = capabilities'),
  );
  const restartCall = reconstruction.indexOf('restartManagedReporterProcess(');
  const activeContext = reconstruction.indexOf("'restarted reporter active context'");
  const traffic = reconstruction.indexOf('runK6Phase({');
  assert.ok(restartCall > 0 && restartCall < activeContext && activeContext < traffic);
  assert.match(runner, /sameRoundPrefix \|\| reconstructed\.roundId > restart\.before\.roundId/);
  assert.match(runner, /jsonb_agg\(jsonb_build_array\(/);
  assert.match(runner, /A\.adScoringPaused\(current\.gameId\)/);
  assert.match(runner, /candidate\.containerId !== target\.containerId/);
  assert.match(runner, /retryTransientUntil\(/);
  assert.match(runner, /replacement container is still transitioning/);
  assert.match(runner, /arenaUrl: `http:\/\//);
  assert.match(runner, /snapshotRows/);
  assert.match(runner, /uniqueCrownRounds/);
  assert.match(runner, /crownMismatches/);
  assert.match(
    restart,
    /waitUntil\([\s\S]*async \(\) => \{[\s\S]*inspectManagedTarget[\s\S]*await exactHealth\(candidate\.arenaUrl/,
  );
  assert.doesNotMatch(restart, /reporterStatus\(/);
  assert.doesNotMatch(restart, /\);\n  await exactHealth\(sameTarget\.arenaUrl/);
});

test('managed KotH traffic is fixed-arrival and gates auth abuse independently', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.doesNotMatch(scenario, /constant-vus/);
  assert.equal(
    [...scenario.matchAll(/preAllocatedVUs: VUS/g)].length,
    2,
    'valid and abuse traffic must preallocate their bounded VU budget',
  );
  assert.doesNotMatch(scenario, /preAllocatedVUs: Math\./);
  assert.match(scenario, /if \(iteration >= TOKENS\.length\) return;/);
  assert.match(scenario, /const token = TOKENS\[iteration\];/);
  assert.doesNotMatch(scenario, /TOKENS\[iteration % TOKENS\.length\]/);
  assert.match(scenario, /valid_capabilities_exercised: \['count==2000'\]/);
  assert.match(scenario, /invalid_capabilities_rate_limited: \['count>0'\]/);
  assert.match(scenario, /invalid_retry_after: \['rate==0'\]/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(scenario, /server_5xx: \['rate==0'\]/);
  assert.match(scenario, /summaryTrendStats: \['avg', 'med', 'p\(90\)', 'p\(95\)', 'p\(99\)', 'max'\]/);
  assert.match(scenario, /valid_play_http_429/);
  assert.match(scenario, /valid_play_model_mismatch/);
  assert.match(scenario, /admin_read_http_429/);
  assert.match(scenario, /admin_read_model_mismatch/);
  assert.match(scenario, /\/api\/edit\/games\/\$\{GAME\}\/ad\/koth\/state/);
  assert.doesNotMatch(scenario, /\/api\/game\/\$\{GAME\}\/ad\/koth\/\$\{CHALLENGE\}\/state/);
});

test('managed KotH workflow validates an exact main candidate and reaps the derived Docker scope', () => {
  assert.match(workflow, /actions: write/);
  assert.match(workflow, /workflow_dispatch:/);
  assert.doesNotMatch(workflow, /pull_request:/);
  assert.match(workflow, /ref: \$\{\{ inputs\.source_sha \}\}/);
  assert.match(workflow, /SOURCE_SHA: \$\{\{ inputs\.source_sha \}\}/);
  assert.match(workflow, /The load candidate must be the exact current main commit/);
  assert.match(workflow, /gh workflow run image\.yml --ref main/);
  assert.match(workflow, /The exact current-main candidate image was not published within 40 minutes/);
  assert.match(workflow, /printf 'explicit\\0%s'/);
  assert.match(workflow, /label=rsctf\.managed=\$managed_scope/);
  assert.match(workflow, /Seed fallback-cleanup probes/);
  assert.match(workflow, /remaining_scope_networks/);
});

test('managed KotH bootstrap carries the stable registration operation identity', () => {
  const bootstrap = workflow.slice(
    workflow.indexOf('- name: Bootstrap isolated administrator'),
    workflow.indexOf('- name: Seed fallback-cleanup probes'),
  );
  assert.match(bootstrap, /operation_id="\$\(cat \/proc\/sys\/kernel\/random\/uuid\)"/);
  assert.match(bootstrap, /--arg operationId "\$operation_id"/);
  assert.match(bootstrap, /operationId: \$operationId/);
  assert.match(bootstrap, /jq -e \. "\$response"/);
});

test('managed KotH polling uses a fresh administrator rate-limit identity', () => {
  const pollingIdentity = runner.slice(
    runner.indexOf('async function provisionPollingAdmin()'),
    runner.indexOf('function targetDatabaseSnapshot('),
  );
  assert.match(pollingIdentity, /\/api\/admin\/users/);
  assert.match(pollingIdentity, /body: \{ role: 'Admin' \}/);
  assert.match(pollingIdentity, /current\.pollerJwt = mintJwt/);
  assert.match(runner, /MANAGED_KOTH_ADMIN_TOKEN: current\.pollerJwt/);
  assert.match(runner, /if \(phase === 'valid'\) await provisionPollingAdmin\(\)/);
  assert.doesNotMatch(runner, /MANAGED_KOTH_ADMIN_TOKEN: A\.adminJwt\(\)/);
});

test('managed target derives its bounded cohort and Crown from submitted scores', () => {
  assert.match(fixture, /scoreable = score > 0/);
  assert.match(fixture, /len\(active_hashes\) >= ACTIVE_FLEET/);
  assert.match(fixture, /len\(active_hashes\) == ACTIVE_FLEET/);
  assert.match(fixture, /sum\(1 for _, score in ranked if score > 0\) != ACTIVE_FLEET/);
  assert.match(fixture, /len\(leaders\) != 1/);
  assert.match(fixture, /selected = ordered/);
});

test('Leaderboard integrity does not confuse snapshot currency with exclusive ownership', () => {
  const integrity = runner.slice(
    runner.indexOf('function integritySnapshot()'),
    runner.indexOf('async function provision('),
  );
  const exclusive = integrity.slice(
    integrity.indexOf("'exclusiveRows'"),
    integrity.indexOf("'duplicateRows'"),
  );
  assert.doesNotMatch(exclusive, /result\.marker_observed/);
  assert.match(exclusive, /controlling_participation_id/);
  assert.match(exclusive, /holder_participation_id/);
  assert.match(exclusive, /KothAcquisitions/);
  assert.match(exclusive, /KothCycleCooldowns/);
});
