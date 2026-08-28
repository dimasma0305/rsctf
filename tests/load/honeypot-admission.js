export const HONEYPOT_BAITS = [
  '/.env',
  '/.git/config',
  '/wp-login.php',
  '/server-status',
];

export const HONEYPOT_AGGREGATE_BURST = 256;
export const HONEYPOT_AGGREGATE_REFILL_PER_SECOND = 4;

export function validDecoyResponse(status, body) {
  return status === 404 && body === 'Not Found';
}

export function maximumAdmittedObservations(durationSeconds) {
  const duration = Number(durationSeconds);
  if (!Number.isFinite(duration) || duration <= 0) throw new Error('duration must be positive');
  return HONEYPOT_AGGREGATE_BURST + Math.ceil(duration * HONEYPOT_AGGREGATE_REFILL_PER_SECOND);
}
