// Read-only fixed-rate monitor gate for bounded capture inventory pages.
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { captureFingerprint, validTrafficRows } from './traffic-inventory.js';
import { mintJwt, runK6, sql, TARGET } from './lib.mjs';

const game = Number(process.env.TRAFFIC_GAME || process.env.GAME);
if (!Number.isSafeInteger(game) || game <= 0) throw new Error('TRAFFIC_GAME (or GAME) is required');
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

async function get(path) {
  const response = await fetch(new URL(path, TARGET), {
    headers: { Authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(10_000),
  });
  const text = await response.text();
  let body;
  try { body = JSON.parse(text); } catch { body = null; }
  if (response.status !== 200) throw new Error(`${path} returned ${response.status}`);
  return { body, bytes: Buffer.byteLength(text) };
}

const summary = await get(`/api/game/games/${game}/captures`);
if (!validTrafficRows(summary.body, 'games', 500, summary.bytes)) throw new Error('invalid capture summary');
const selected = process.env.CID
  ? summary.body.find((row) => row.id === Number(process.env.CID))
  : summary.body.find((row) => row.count > 0);
if (!selected) throw new Error(`game ${game} has no selected challenge with capture inventory`);
const teams = await get(`/api/game/captures/${selected.id}?count=100&skip=0`);
if (!validTrafficRows(teams.body, 'teams', 100, teams.bytes) || teams.body.length === 0) {
  throw new Error(`challenge ${selected.id} has no valid capture team page`);
}
const selectedTeam = process.env.PID
  ? teams.body.find((row) => row.id === Number(process.env.PID))
  : teams.body[0];
if (!selectedTeam) throw new Error('selected capture participation is not present on the first bounded page');
const files = await get(`/api/game/captures/${selected.id}/${selectedTeam.id}?count=100&skip=0`);
if (!validTrafficRows(files.body, 'files', 100, files.bytes) || files.body.length === 0) {
  throw new Error('selected participation has no valid PCAP file page');
}
const before = [captureFingerprint(summary.body), captureFingerprint(teams.body), captureFingerprint(files.body)].join('\n');

const directory = mkdtempSync(join(tmpdir(), 'rsctf-traffic-inventory-'));
const tokenFile = join(directory, 'monitor-token.json');
writeFileSync(tokenFile, JSON.stringify(tokens), { mode: 0o600 });
let status = 1;
try {
  status = runK6('traffic-inventory.js', {
    TARGET, GAME: game, CID: selected.id, PID: selectedTeam.id, TOKENS_FILE: tokenFile,
    RATE: process.env.RATE || 2, VUS: process.env.VUS || 8,
    DURATION: process.env.DURATION || '30s', SUMMARY_JSON: process.env.SUMMARY_JSON || '',
  });
} finally {
  rmSync(directory, { recursive: true, force: true });
}
if (status !== 0) process.exit(status);
const [afterSummary, afterTeams, afterFiles] = await Promise.all([
  get(`/api/game/games/${game}/captures`),
  get(`/api/game/captures/${selected.id}?count=100&skip=0`),
  get(`/api/game/captures/${selected.id}/${selectedTeam.id}?count=100&skip=0`),
]);
const after = [captureFingerprint(afterSummary.body), captureFingerprint(afterTeams.body), captureFingerprint(afterFiles.body)].join('\n');
if (after !== before) throw new Error('read-only traffic inventory changed during the run');
console.log(`traffic_inventory_ok game=${game} challenge=${selected.id} participation=${selectedTeam.id}`);
