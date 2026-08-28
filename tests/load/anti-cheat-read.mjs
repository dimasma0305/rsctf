// Read-only large-ledger gate for the cursor incident feed and conditional report.
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { ledgerFingerprint, positiveInteger, validIncidentPage } from './anti-cheat-read.js';
import { mintJwt, runK6, sql, TARGET } from './lib.mjs';

const game = positiveInteger(process.env.ANTI_CHEAT_GAME || process.env.GAME, 'ANTI_CHEAT_GAME');
const minimumIncidents = positiveInteger(process.env.MIN_INCIDENTS || 10_000, 'MIN_INCIDENTS');
if (minimumIncidents > 10_000) throw new Error('MIN_INCIDENTS cannot exceed the report safety limit of 10000');

function fingerprint() {
  return ledgerFingerprint(sql(
    `SELECT COUNT(*)::text || '|' || COALESCE(MAX(id),0)::text || '|' || ` +
      `(SELECT COUNT(*) FROM "FirstSolves" solve JOIN "Participations" p ON p.id=solve.participation_id WHERE p.game_id=${game})::text || '|' || ` +
      `(SELECT COUNT(*) FROM "SuspicionEvents" WHERE game_id=${game})::text || '|' || ` +
      `(SELECT COUNT(*) FROM "IdentityObservations" WHERE game_id=${game})::text || '|' || ` +
      `(SELECT COUNT(*) FROM "SuspicionEvaluationOutbox" WHERE game_id=${game})::text ` +
      `FROM "CheatInfo" WHERE game_id=${game}`,
  ));
}

const before = fingerprint();
const incidentCount = Number(before.split('|')[0]);
if (incidentCount < minimumIncidents || incidentCount > 10_000) {
  throw new Error(
    `anti-cheat-read requires ${minimumIncidents}-10000 immutable incidents in game ${game}; found ${incidentCount}`,
  );
}

const accounts = sql(
  `SELECT id::text || '|' || security_stamp || '|' || role::text FROM "AspNetUsers" ` +
    `WHERE role IN (2,3) AND security_stamp IS NOT NULL ORDER BY role DESC,id LIMIT 16`,
).split('\n').filter(Boolean);
if (!accounts.length) throw new Error('one Monitor/Admin account is required');
const tokens = process.env.MONITOR_TOKEN
  ? [process.env.MONITOR_TOKEN]
  : accounts.map((account) => {
      const [id, stamp, role] = account.split('|');
      return mintJwt(id, stamp, Number(role));
    });
const token = tokens[0];

async function request(path, headers = {}) {
  const response = await fetch(new URL(path, TARGET), {
    headers: { Authorization: `Bearer ${token}`, ...headers },
    signal: AbortSignal.timeout(10_000),
  });
  const text = await response.text();
  return { response, text };
}

const page = await request(`/api/game/${game}/cheatinfo/page?count=100`);
if (page.response.status !== 200 || !validIncidentPage(JSON.parse(page.text), 0, Buffer.byteLength(page.text), false)) {
  throw new Error(`anti-cheat incident preflight failed with HTTP ${page.response.status}`);
}
const pageModel = JSON.parse(page.text);
const deltaAfter = Math.max(0, pageModel.nextCursor - 100);
const report = await request(`/api/game/${game}/cheatreport`);
if (report.response.status !== 200) throw new Error(`anti-cheat report preflight returned ${report.response.status}`);
const etag = report.response.headers.get('etag');
if (!etag) throw new Error('anti-cheat report did not publish an ETag');

const directory = mkdtempSync(join(tmpdir(), 'rsctf-anti-cheat-read-'));
const tokenFile = join(directory, 'monitor-token.json');
writeFileSync(tokenFile, JSON.stringify(tokens), { mode: 0o600 });
let status = 1;
try {
  status = runK6('anti-cheat-read.js', {
    TARGET,
    GAME: game,
    TOKENS_FILE: tokenFile,
    REPORT_ETAG: etag,
    DELTA_AFTER: deltaAfter,
    RATE: process.env.RATE || 2,
    VUS: process.env.VUS || 8,
    DURATION: process.env.DURATION || '30s',
    SUMMARY_JSON: process.env.SUMMARY_JSON || '',
  });
} finally {
  rmSync(directory, { recursive: true, force: true });
}
if (status !== 0) process.exit(status);
const after = fingerprint();
if (after !== before) throw new Error(`read-only anti-cheat load changed ledger ${before} -> ${after}`);
console.log(`anti_cheat_read_ok game=${game} incidents=${incidentCount} fingerprint=${after}`);
