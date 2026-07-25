// Mint a broad disposable-user cohort and run the fixed-rate read-only smoke.
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { mintJwt, runK6, sql, TARGET } from './lib.mjs';

const JEO_GAME = Number(process.env.JEO_GAME);
const AD_GAME = Number(process.env.AD_GAME || process.env.GAME);
const TOKEN_COUNT = Number(process.env.TOKEN_COUNT || 1000);
const TOKEN_OFFSET = Number(process.env.TOKEN_OFFSET || 0);

if (
  !Number.isSafeInteger(JEO_GAME) ||
  JEO_GAME <= 0 ||
  !Number.isSafeInteger(AD_GAME) ||
  AD_GAME <= 0
) {
  throw new Error('positive JEO_GAME and AD_GAME (or GAME) are required');
}
if (!Number.isSafeInteger(TOKEN_COUNT) || TOKEN_COUNT < 100 || TOKEN_COUNT > 4000) {
  throw new Error('TOKEN_COUNT must be between 100 and 4000');
}
if (!Number.isSafeInteger(TOKEN_OFFSET) || TOKEN_OFFSET < 0) {
  throw new Error('TOKEN_OFFSET must be a non-negative integer');
}

const accounts = sql(
  `SELECT id::text || '|' || security_stamp
     FROM "AspNetUsers"
    WHERE security_stamp IS NOT NULL
      AND (user_name LIKE 'LT_%' OR user_name LIKE 'LOADTEST%' OR email LIKE '%@load.test')
    ORDER BY id
    LIMIT ${TOKEN_COUNT}
   OFFSET ${TOKEN_OFFSET}`,
)
  .split('\n')
  .filter(Boolean);
if (accounts.length < 100) {
  throw new Error(`at least 100 disposable load-test accounts are required; found ${accounts.length}`);
}

const tokens = accounts.map((entry) => {
  const [id, stamp] = entry.split('|');
  return mintJwt(id, stamp, 1);
});

console.log(
  `polled read load → ${TARGET} jeo=${JEO_GAME} ad=${AD_GAME} ` +
    `rate=${process.env.RATE || 300}/s tokens=${tokens.length}`,
);
const tokenDirectory = mkdtempSync(join(tmpdir(), 'rsctf-polled-read-'));
const tokenFile = join(tokenDirectory, 'tokens.json');
writeFileSync(tokenFile, JSON.stringify(tokens), { mode: 0o600 });
let status = 1;
try {
  status = runK6('polled-read.js', {
    TARGET,
    JEO_GAME,
    AD_GAME,
    TOKENS_FILE: tokenFile,
    RATE: process.env.RATE || 300,
    VUS: process.env.VUS || 100,
    DURATION: process.env.DURATION || '60s',
    SUMMARY_JSON: process.env.SUMMARY_JSON || '',
  });
} finally {
  rmSync(tokenDirectory, { recursive: true, force: true });
}
process.exit(status);
