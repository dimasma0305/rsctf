// Read-only fixed-rate gate for the organizer's maintenance status poll.
import assert from 'node:assert/strict';
import { GAME, TARGET, mintJwt, runK6, sql } from './lib.mjs';

const gameId = Number(GAME);
assert.ok(Number.isSafeInteger(gameId) && gameId > 0, 'GAME must be a positive integer');
assert.ok(['127.0.0.1', 'localhost', '[::1]'].includes(new URL(TARGET).hostname), 'use a disposable local stack');
assert.match(sql(`SELECT title FROM "Games" WHERE id=${gameId}`), /^LIVE-MAINTENANCE-LOAD-/);
const [adminId, stamp] = sql(`SELECT id::text || '|' || security_stamp FROM "AspNetUsers"
  WHERE role=3 AND security_stamp IS NOT NULL ORDER BY id LIMIT 1`).split('|');
assert.ok(adminId && stamp, 'a disposable administrator is required');

function integrity() {
  return sql(`SELECT jsonb_build_array(
    (SELECT md5(COALESCE(jsonb_agg(to_jsonb(w) ORDER BY id)::text,'[]'))
       FROM "CompetitionMaintenanceWindows" w WHERE game_id=${gameId}),
    (SELECT md5(COALESCE(jsonb_agg(to_jsonb(r) ORDER BY id)::text,'[]'))
       FROM "AdRounds" r WHERE game_id=${gameId}),
    (SELECT md5(COALESCE(jsonb_agg(to_jsonb(r) ORDER BY id)::text,'[]'))
       FROM "KothControlResults" r WHERE game_id=${gameId}),
    (SELECT COUNT(*) FROM (SELECT game_id FROM "CompetitionMaintenanceWindows"
       WHERE game_id=${gameId} AND state IN ('Requested','Editing','Applying')
       GROUP BY game_id HAVING COUNT(*)>1) duplicates))`);
}
const before = integrity();
assert.equal(JSON.parse(before)[3], 0, 'duplicate active maintenance windows');
const status = runK6('competition-maintenance.js', {
  TARGET, GAME: gameId, ADMIN_TOKEN: mintJwt(adminId, stamp),
  RATE: process.env.RATE || 2, DURATION: process.env.DURATION || '30s',
});
assert.equal(integrity(), before, 'maintenance polling changed retained windows or scoring evidence');
process.exitCode = status;
