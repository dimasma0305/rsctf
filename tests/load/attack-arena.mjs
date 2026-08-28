// Run the public arena cycle at a held arrival rate. This is intentionally
// read-only and requires an explicit acknowledgement before stressing a host.
import { runK6, sql, TARGET } from './lib.mjs';

const game = Number(process.env.ATTACK_ARENA_GAME || process.env.GAME);
if (!Number.isSafeInteger(game) || game <= 0) throw new Error('ATTACK_ARENA_GAME (or GAME) is required');
if (process.env.ATTACK_ARENA_LOAD_ACK !== '1') throw new Error('set ATTACK_ARENA_LOAD_ACK=1 for this fixed-rate gate');

const targetUrl = new URL(TARGET);
if (!['127.0.0.1', 'localhost', '::1'].includes(targetUrl.hostname) && process.env.ALLOW_REMOTE_ATTACK_ARENA_LOAD !== targetUrl.origin) {
  throw new Error(`remote target requires ALLOW_REMOTE_ATTACK_ARENA_LOAD=${targetUrl.origin}`);
}

const livePublic = () => sql(
  `SELECT COUNT(*) FROM "Games" WHERE id=${game} AND hidden=FALSE ` +
    `AND vpn_access_required=FALSE ` +
    `AND start_time_utc<=clock_timestamp() AND end_time_utc>=clock_timestamp()`,
);
if (livePublic() !== '1') throw new Error(`game ${game} must be live, public, and not Event-VPN-locked for the arena load gate`);

const status = runK6('attack-arena.js', {
  TARGET,
  GAME: game,
  RATE: process.env.RATE || 80,
  VUS: process.env.VUS || 120,
  MAX_VUS: process.env.MAX_VUS || 480,
  DURATION: process.env.DURATION || '60s',
  SUMMARY_JSON: process.env.SUMMARY_JSON || '',
});
if (livePublic() !== '1') throw new Error(`game ${game} lost its public lifecycle invariant during the arena load gate`);
process.exit(status);
