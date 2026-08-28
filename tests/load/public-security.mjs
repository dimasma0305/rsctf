// Fixed-rate read-only gate for anonymous HashPoW issuance and authoritative
// team-credential verification. The runner discovers one real live credential;
// it never creates, rotates, or edits production state.
import crypto from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { runK6, sql, TARGET } from './lib.mjs';

const game = Number(process.env.PUBLIC_SECURITY_GAME || process.env.GAME);
const team = Number(process.env.PUBLIC_SECURITY_TEAM);
if (!Number.isSafeInteger(game) || game <= 0) throw new Error('PUBLIC_SECURITY_GAME (or GAME) is required');
if (!Number.isSafeInteger(team) || team <= 0) throw new Error('PUBLIC_SECURITY_TEAM is required');
if (process.env.PUBLIC_SECURITY_STRESS_ACK !== '1') {
  throw new Error('set PUBLIC_SECURITY_STRESS_ACK=1 for the public security stress gate');
}
const origin = new URL(TARGET).origin;
if (!['127.0.0.1', 'localhost', '::1'].includes(new URL(TARGET).hostname) &&
    process.env.ALLOW_REMOTE_PUBLIC_SECURITY_STRESS !== origin) {
  throw new Error(`remote target requires ALLOW_REMOTE_PUBLIC_SECURITY_STRESS=${origin}`);
}

const row = sql(
  `SELECT row_to_json(scope)::text FROM (` +
    `SELECT game.public_key AS "publicKey", participation.token AS "teamToken" ` +
    `FROM "Games" game JOIN "Participations" participation ON participation.game_id=game.id ` +
    `JOIN "Teams" team ON team.id=participation.team_id ` +
    `WHERE game.id=${game} AND participation.team_id=${team} AND participation.status=1 ` +
    `AND NOT game.deletion_pending AND NOT team.deletion_pending ` +
    `AND game.start_time_utc<=clock_timestamp() AND clock_timestamp()<game.end_time_utc) scope`,
);
if (!row) throw new Error(`game ${game}, team ${team} is not a live Accepted credential scope`);
const trusted = JSON.parse(row);

const attacker = crypto.generateKeyPairSync('ed25519');
const attackerKey = Buffer.from(attacker.publicKey.export({ type: 'spki', format: 'der' })).subarray(-32).toString('base64');
const attackerSignature = crypto.sign(null, Buffer.from(`RSCTF_TEAM_${team}`), attacker.privateKey).toString('base64');
const fixture = {
  trusted,
  attacker: { publicKey: attackerKey, teamToken: `${team}:${attackerSignature}` },
};

const captcha = await fetch(new URL('/api/captcha', TARGET));
if (captcha.status !== 200) throw new Error(`captcha discovery returned ${captcha.status}`);
const captchaModel = await captcha.json();
if ((captchaModel?.type ?? captchaModel?.data?.type) !== 'HashPow') {
  throw new Error('public security gate requires enabled HashPow captcha');
}
const probe = await fetch(new URL('/api/captcha/powchallenge', TARGET));
if (probe.status !== 200) throw new Error(`HashPoW issuance probe returned ${probe.status}`);
if (!/(?:^|,)\s*no-store(?:\s*(?:,|$))/i.test(probe.headers.get('cache-control') || '')) {
  throw new Error('HashPoW issuance must be explicitly no-store');
}

function fingerprint() {
  return sql(
    `SELECT md5(COALESCE(string_agg(value,'|' ORDER BY value),'')) FROM (` +
      `SELECT id::text || ':' || public_key AS value FROM "Games" WHERE id=${game} UNION ALL ` +
      `SELECT id::text || ':' || team_id::text || ':' || status::text || ':' || token ` +
      `FROM "Participations" WHERE game_id=${game} AND team_id=${team}) rows`,
  );
}

const before = fingerprint();
const directory = mkdtempSync(join(tmpdir(), 'rsctf-public-security-'));
const fixtureFile = join(directory, 'fixture.json');
writeFileSync(fixtureFile, JSON.stringify(fixture), { mode: 0o600 });
try {
  const status = runK6('public-security.js', {
    TARGET,
    FIXTURE_FILE: fixtureFile,
    RATE: process.env.RATE || '8',
    VUS: process.env.VUS || '24',
    DURATION: process.env.DURATION || '30s',
    SUMMARY_JSON: process.env.SUMMARY_JSON || '',
  });
  if (status !== 0) throw new Error(`public security fixed-rate gate failed with exit ${status}`);
} finally {
  rmSync(directory, { recursive: true, force: true });
}
if (fingerprint() !== before) throw new Error('public security load changed game credential state');
console.log(`public_security_ok game=${game} team=${team}`);
