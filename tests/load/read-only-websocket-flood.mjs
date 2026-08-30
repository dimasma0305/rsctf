// Fixed-rate application-frame flood against the public read-only attack feeds.
import { runK6, sql, TARGET } from './lib.mjs';

const game = Number(process.env.WEBSOCKET_GAME || process.env.GAME);
if (!Number.isSafeInteger(game) || game <= 0) throw new Error('WEBSOCKET_GAME (or GAME) is required');
const targetUrl = new URL(TARGET);
if (process.env.READONLY_WS_FLOOD_ACK !== '1') throw new Error('set READONLY_WS_FLOOD_ACK=1 for this inbound-abuse gate');
if (!['127.0.0.1', 'localhost', '::1'].includes(targetUrl.hostname) && process.env.ALLOW_REMOTE_READONLY_WS_FLOOD !== targetUrl.origin) {
  throw new Error(`remote target requires ALLOW_REMOTE_READONLY_WS_FLOOD=${targetUrl.origin}`);
}
const livePublic = () => sql(
  `SELECT COUNT(*) FROM "Games" WHERE id=${game} AND hidden=FALSE ` +
    `AND start_time_utc<=clock_timestamp() AND end_time_utc>=clock_timestamp()`,
);
if (livePublic() !== '1') throw new Error(`game ${game} must be live and public for the read-only feed drill`);
const status = runK6('read-only-websocket-flood.js', {
  TARGET, GAME: game, RATE: process.env.RATE || 20, VUS: process.env.VUS || 40,
  MAX_VUS: process.env.MAX_VUS || 160, DURATION: process.env.DURATION || '30s',
  FRAME_BYTES: process.env.FRAME_BYTES || 65_536, SUMMARY_JSON: process.env.SUMMARY_JSON || '',
});
if (livePublic() !== '1') throw new Error(`game ${game} lost its public lifecycle invariant during the read-only feed drill`);
process.exit(status);
