import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/anticheat-reconciliation.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../anticheat-reconciliation.mjs', import.meta.url), 'utf8');

test('anti-cheat idle gate keeps scoreboard and exact health on fixed arrival rates', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /\/api\/game\/\$\{GAME\}\/scoreboard/);
  assert.match(scenario, /Authorization: `Bearer \$\{token\}`/);
  assert.match(scenario, /response\.body !== 'ok'/);
  assert.match(scenario, /anticheat_scoreboard_ms: \['p\(95\)<750'\]/);
  assert.match(scenario, /anticheat_health_ms: \['p\(95\)<500'\]/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
});

test('runner proves large-history idle work is zero, operations coalesce, and resources stay bounded', () => {
  assert.match(runner, /ANTICHEAT_RECONCILIATION_STRESS_ACK/);
  assert.match(runner, /ALLOW_REMOTE_ANTICHEAT_RECONCILIATION_STRESS/);
  assert.match(runner, /MIN_ANTICHEAT_HISTORY/);
  assert.match(runner, /"SuspicionEvaluationOutbox"/);
  assert.match(runner, /"IdentityObservations"/);
  assert.match(runner, /desired_generation=desired_generation\+1/);
  assert.match(runner, /Promise\.all/);
  assert.match(runner, /adminTokens\[index % adminTokens\.length\]/);
  assert.match(runner, /new Set\(jobIds\)\.size !== 1/);
  assert.match(runner, /"ControlPlaneJobOperations"/);
  assert.match(runner, /afterManual\.attempts !== beforeManual\.attempts \+ 1/);
  assert.match(runner, /idle reconciliation started another pass/);
  assert.match(runner, /idleAfter\[key\] !== idleBaseline\[key\]/);
  assert.match(runner, /docker', \['stats'/);
  assert.match(runner, /docker', \['top'/);
  assert.match(runner, /CPUPerc/);
  assert.match(runner, /MAX_CPU_PERCENT/);
  assert.match(runner, /pg_stat_activity/);
  assert.match(runner, /pg_stat_database/);
  assert.match(runner, /poolConnections/);
  assert.match(runner, /activeConnections/);
  assert.match(runner, /idleInTransactionConnections/);
  assert.match(runner, /state LIKE 'idle in transaction%'/);
  assert.match(runner, /waitingConnections/);
  assert.match(runner, /longestTransactionSeconds/);
  assert.match(runner, /MAX_PG_CONNECTIONS/);
  assert.match(runner, /MAX_PG_BLOCK_READ_DELTA/);
  assert.match(runner, /MAX_PG_TEMP_DELTA_MIB/);
  assert.match(runner, /databaseSamples\.length < 2/);
  assert.match(runner, /peakCpuPercent > limits\.cpuPercent/);
  assert.match(runner, /peakConnections > limits\.pgConnections/);
  assert.match(runner, /peakActiveConnections > limits\.pgActiveConnections/);
  assert.match(runner, /peakIdleInTransaction > limits\.pgIdleInTransaction/);
  assert.match(runner, /peakWaitingConnections > limits\.pgWaitingConnections/);
  assert.match(runner, /longestTransactionSeconds > limits\.pgLongestTransactionSeconds/);
  assert.match(runner, /blockReadDelta < 0 \|\| blockReadDelta > limits\.pgBlockReads/);
  assert.match(runner, /tempByteDelta < 0 \|\| tempByteDelta > limits\.pgTempMiB/);
  assert.match(runner, /mode: 0o600/);
  assert.match(runner, /rmSync\(fixtureDirectory, \{ recursive: true, force: true \}\)/);
  assert.match(runner, /body !== 'ok'/);
});
