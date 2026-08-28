// Destructive manual/scheduled coalescing gate for a disposable large ledger.
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { randomBytes } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { mintJwt, positiveInteger, runK6, sql, TARGET } from './lib.mjs';

if (process.env.ANTI_CHEAT_RECONCILE_STRESS_ACK !== '1') {
  throw new Error('ANTI_CHEAT_RECONCILE_STRESS_ACK=1 is required for durable manual operations');
}
const game = positiveInteger(process.env.ANTI_CHEAT_GAME || process.env.GAME, 'ANTI_CHEAT_GAME');
const minimumRows = positiveInteger(process.env.MIN_SOURCE_ROWS || 10_000, 'MIN_SOURCE_ROWS');

function sourceSnapshot() {
  const value = sql(
    `SELECT (SELECT COUNT(*) FROM "IdentityObservations" WHERE game_id=${game})::text || '|' || ` +
    `(SELECT COUNT(*) FROM "VpnDnsProviderBuckets" WHERE game_id=${game})::text || '|' || ` +
    `(SELECT COUNT(*) FROM "VpnPeerNetworkObservations" WHERE game_id=${game})::text || '|' || ` +
    `(SELECT COUNT(*) FROM "VpnFlagTransportEvents" WHERE game_id=${game})::text || '|' || ` +
    `(SELECT COUNT(*) FROM "CheatInfo" WHERE game_id=${game})::text || '|' || ` +
    `COALESCE((SELECT MAX(id) FROM "IdentityObservations" WHERE game_id=${game}), 0)::text || '|' || ` +
    `COALESCE((SELECT MAX(reconcile_revision) FROM "VpnDnsProviderBuckets" WHERE game_id=${game}), 0)::text || '|' || ` +
    `COALESCE((SELECT MAX(reconcile_revision) FROM "VpnPeerNetworkObservations" WHERE game_id=${game}), 0)::text || '|' || ` +
    `COALESCE((SELECT MAX(id) FROM "VpnFlagTransportEvents" WHERE game_id=${game}), 0)::text || '|' || ` +
    `COALESCE((SELECT MAX(id) FROM "CheatInfo" WHERE game_id=${game}), 0)::text`,
  );
  const fields = value.split('|').map(Number);
  if (fields.length !== 10 || fields.some((field) => !Number.isSafeInteger(field) || field < 0)) {
    throw new Error(`invalid source snapshot ${value}`);
  }
  const counts = fields.slice(0, 5);
  return { value, counts, through: fields.slice(5), total: counts.reduce((sum, count) => sum + count, 0) };
}

function cursorSnapshot() {
  return sql(
    `SELECT COALESCE(watermark.identity_observation_id, 0)::text || '|' || ` +
    `COALESCE(watermark.dns_revision, 0)::text || '|' || ` +
    `COALESCE(watermark.network_revision, 0)::text || '|' || ` +
    `COALESCE(watermark.flag_transport_id, 0)::text || '|' || ` +
    `COALESCE(watermark.cheat_info_id, 0)::text || '|' || ` +
    `COALESCE(reconciliation.dirty_generation, 0)::text || '|' || ` +
    `COALESCE(reconciliation.completed_generation, 0)::text || '|' || ` +
    `COALESCE(reconciliation.dirty_mask, 0)::text ` +
    `FROM "Games" game ` +
    `LEFT JOIN "SuspicionReconciliationState" reconciliation ON reconciliation.game_id=game.id ` +
    `LEFT JOIN "SuspicionReconciliationWatermarks" watermark ON watermark.game_id=game.id ` +
    `WHERE game.id=${game}`,
  );
}

async function exactHealth() {
  const response = await fetch(new URL('/healthz', TARGET), { signal: AbortSignal.timeout(5000) });
  const body = await response.text();
  if (response.status !== 200 || body !== 'ok') throw new Error(`healthz failed: ${response.status} ${body}`);
}

const before = sourceSnapshot();
if (before.total < minimumRows) {
  throw new Error(`large-ledger reconciliation requires ${minimumRows} source rows; found ${before.total}`);
}
const accounts = sql(
  `SELECT id::text || '|' || security_stamp FROM "AspNetUsers" ` +
  `WHERE role=3 AND security_stamp IS NOT NULL ORDER BY id LIMIT 16`,
).split('\n').filter(Boolean);
if (!accounts.length) throw new Error('an Admin account is required');
const tokens = accounts.map((account) => {
  const [id, stamp] = account.split('|');
  return mintJwt(id, stamp, 3);
});

await exactHealth();
const cursorBefore = cursorSnapshot();
const directory = mkdtempSync(join(tmpdir(), 'rsctf-anti-cheat-reconcile-'));
const tokenFile = join(directory, 'admin-tokens.json');
const summaryFile = process.env.SUMMARY_JSON || join(directory, 'summary.json');
writeFileSync(tokenFile, JSON.stringify(tokens), { mode: 0o600 });
let status = 1;
try {
  status = runK6('anti-cheat-reconcile.js', {
    TARGET,
    GAME: game,
    TOKENS_FILE: tokenFile,
    OPERATION_PREFIX: randomBytes(4).toString('hex'),
    RATE: process.env.RATE || 2,
    VUS: process.env.VUS || 8,
    DURATION: process.env.DURATION || '30s',
    SUMMARY_JSON: summaryFile,
  });
  if (status !== 0) process.exitCode = status;
  if (status === 0) {
    const deadline = Date.now() + 60_000;
    let state = cursorSnapshot();
    while (Date.now() < deadline) {
      const fields = state.split('|').map(Number);
      if (fields.length === 8 && fields[5] <= fields[6] && fields[7] === 0) break;
      await new Promise((resolve) => setTimeout(resolve, 1000));
      state = cursorSnapshot();
    }
    const fields = state.split('|').map(Number);
    if (fields.length !== 8 || fields[5] > fields[6] || fields[7] !== 0) {
      throw new Error(`reconciliation did not become idle: ${state}`);
    }
    if (fields.slice(0, 5).some((cursor, index) => cursor !== before.through[index])) {
      throw new Error(`idle reconciliation left source cursors behind: ${state}`);
    }
    await exactHealth();
    const after = sourceSnapshot();
    if (after.value !== before.value) throw new Error(`source ledger changed ${before.value} -> ${after.value}`);
    const summary = JSON.parse(readFileSync(summaryFile, 'utf8'));
    console.log(JSON.stringify({ game, sourceRows: before.total, cursorBefore, cursorAfter: state, summary }, null, 2));
  }
} finally {
  rmSync(directory, { recursive: true, force: true });
}
