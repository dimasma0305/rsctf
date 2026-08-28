import { createHash } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { requirePersonalToken } from './personal-token-admission.js';
import { runK6, sql, TARGET } from './lib.mjs';

const target = new URL(TARGET);
if (process.env.PERSONAL_TOKEN_STRESS_ACK !== '1') {
  throw new Error('set PERSONAL_TOKEN_STRESS_ACK=1 for the managed-token authentication gate');
}
if (!['127.0.0.1', 'localhost', '::1'].includes(target.hostname) &&
    process.env.ALLOW_REMOTE_PERSONAL_TOKEN_STRESS !== target.origin) {
  throw new Error(`remote target requires ALLOW_REMOTE_PERSONAL_TOKEN_STRESS=${target.origin}`);
}
const valid = requirePersonalToken(process.env.VALID_PERSONAL_TOKEN, 'VALID_PERSONAL_TOKEN');
const revoked = requirePersonalToken(process.env.REVOKED_PERSONAL_TOKEN, 'REVOKED_PERSONAL_TOKEN');
if (valid === revoked) throw new Error('VALID_PERSONAL_TOKEN and REVOKED_PERSONAL_TOKEN must differ');
const digest = (token) => createHash('sha256').update(token).digest('hex');
const literal = (value) => String(value).replaceAll("'", "''");

const validRows = Number(sql(
  `SELECT COUNT(*) FROM "ApiTokens" token JOIN "AspNetUsers" account ON account.id=token.creator_id ` +
  `WHERE token.token_hash='${literal(digest(valid))}' AND NOT token.is_revoked ` +
  `AND (token.expires_at IS NULL OR token.expires_at>clock_timestamp()) ` +
  `AND token.audience='admin_api' AND token.security_stamp_hash IS NOT NULL ` +
  `AND account.security_stamp IS NOT NULL AND account.role=3`,
));
if (validRows !== 1) throw new Error('VALID_PERSONAL_TOKEN is not one live admin credential');
const revokedRows = Number(sql(
  `SELECT COUNT(*) FROM "ApiTokens" WHERE token_hash='${literal(digest(revoked))}' AND is_revoked`,
));
if (revokedRows !== 1) throw new Error('REVOKED_PERSONAL_TOKEN is not a revoked credential');

const fingerprint = () => sql(
  `SELECT COALESCE(string_agg(id::text || ':' || token_hash || ':' || is_revoked::text || ':' || audience || ':' || ` +
  `COALESCE(security_stamp_hash,'') || ':' || COALESCE(extract(epoch FROM expires_at)::text,''),',' ORDER BY id),'') ` +
  `FROM "ApiTokens"`,
);
const before = fingerprint();
const directory = mkdtempSync(join(tmpdir(), 'rsctf-personal-token-'));
const fixtureFile = join(directory, 'tokens.json');
writeFileSync(fixtureFile, JSON.stringify({ valid, revoked }), { mode: 0o600 });
try {
  const status = runK6('personal-token-admission.js', {
    TARGET,
    TOKENS_FILE: fixtureFile,
    RATE: process.env.RATE || 10,
    VUS: process.env.VUS || 24,
    DURATION: process.env.DURATION || '20s',
    SUMMARY_JSON: process.env.SUMMARY_JSON || '',
  });
  if (status !== 0) throw new Error(`personal-token admission gate failed with exit ${status}`);
} finally {
  rmSync(directory, { recursive: true, force: true });
}
if (fingerprint() !== before) throw new Error('managed-token load changed credential authority');
console.log('personal_token_admission_ok');
