// Fixed-rate public-decoy gate. It requires a disposable/local deployment
// because the expected result is bounded telemetry written to PostgreSQL.
import { randomUUID } from 'node:crypto';

import { maximumAdmittedObservations } from './honeypot-admission.js';
import { runK6, sleep, sql, TARGET } from './lib.mjs';

const literal = (value) => `'${String(value).replaceAll("'", "''")}'`;

const duration = String(process.env.DURATION || '10s');
const durationMatch = duration.match(/^(\d+)s$/);
if (!durationMatch) throw new Error('DURATION must use whole seconds, for example 10s');
const durationSeconds = Number(durationMatch[1]);
const targetUrl = new URL(TARGET);
if (process.env.HONEYPOT_STRESS_ACK !== '1') {
  throw new Error('set HONEYPOT_STRESS_ACK=1 for the public decoy stress gate');
}
if (
  !['127.0.0.1', 'localhost', '::1'].includes(targetUrl.hostname) &&
  process.env.ALLOW_REMOTE_HONEYPOT_STRESS !== targetUrl.origin
) {
  throw new Error(`remote target requires ALLOW_REMOTE_HONEYPOT_STRESS=${targetUrl.origin}`);
}

const marker = `rsctf-honeypot-load/${randomUUID()}`;
const rawBefore = Number(
  sql(`SELECT COUNT(*) FROM "HoneypotHits" WHERE user_agent=${literal(marker)}`),
);
const status = runK6('honeypot-admission.js', {
  TARGET,
  MARKER: marker,
  RATE: process.env.RATE || 60,
  VUS: process.env.VUS || 24,
  DURATION: duration,
  SUMMARY_JSON: process.env.SUMMARY_JSON || '',
});
if (status !== 0) process.exit(status);

let snapshot = { rows: 0, hits: 0 };
let stable = 0;
for (let attempt = 0; attempt < 40 && stable < 3; attempt += 1) {
  const row = sql(
    `SELECT COUNT(*)::text || '|' || COALESCE(SUM(hit_count),0)::text ` +
      `FROM "HoneypotHitBuckets" WHERE user_agent=${literal(marker)}`,
  );
  const [rows, hits] = row.split('|').map(Number);
  const next = { rows, hits };
  stable =
    next.hits > 0 && next.rows === snapshot.rows && next.hits === snapshot.hits
      ? stable + 1
      : 0;
  snapshot = next;
  await sleep(250);
}

const maximum = maximumAdmittedObservations(durationSeconds) + 8;
if (snapshot.hits <= 0 || snapshot.hits > maximum) {
  throw new Error(`honeypot admitted observations escaped the aggregate bound: ${JSON.stringify({ snapshot, maximum })}`);
}
if (snapshot.rows <= 0 || snapshot.rows > snapshot.hits || snapshot.rows > maximum) {
  throw new Error(`honeypot bucket cardinality is invalid: ${JSON.stringify({ snapshot, maximum })}`);
}
const rawAfter = Number(
  sql(`SELECT COUNT(*) FROM "HoneypotHits" WHERE user_agent=${literal(marker)}`),
);
if (rawAfter !== rawBefore) throw new Error('public decoys regressed to one-row-per-hit storage');
const retained = Number(sql(`SELECT row_count FROM "HoneypotBucketBudget" WHERE singleton=TRUE`));
if (!Number.isSafeInteger(retained) || retained < snapshot.rows || retained > 250000) {
  throw new Error(`honeypot retained-row budget is invalid: ${retained}`);
}
console.log(`honeypot_admission_ok rows=${snapshot.rows} hits=${snapshot.hits} retained=${retained}`);
