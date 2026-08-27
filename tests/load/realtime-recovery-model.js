/** Conservative fixed-rate model for browser HTTP backfills. The client uses
 * ±10% jitter, so the shortest legal period (90%) is the request-count upper
 * bound. `handshakeBackfills` includes the initial page read plus the
 * authoritative read after a successful hub handshake. */
export function fallbackRequestUpperBound({
  clients,
  durationMs,
  pollingIntervalMs,
  handshakeBackfills = 2,
}) {
  for (const [name, value] of Object.entries({
    clients,
    durationMs,
    pollingIntervalMs,
    handshakeBackfills,
  })) {
    if (!Number.isSafeInteger(value) || value < 0)
      throw new Error(`${name} must be a non-negative integer`);
  }
  if (clients === 0 || durationMs === 0) return 0;
  if (pollingIntervalMs === 0) return clients * handshakeBackfills;
  const shortestPollMs = pollingIntervalMs * 0.9;
  const pollsPerClient = Math.floor(durationMs / shortestPollMs);
  return clients * (handshakeBackfills + pollsPerClient);
}

export function steadyFallbackRequestsPerSecond(clients, pollingIntervalMs) {
  if (!Number.isSafeInteger(clients) || clients < 0)
    throw new Error("clients must be a non-negative integer");
  if (!Number.isSafeInteger(pollingIntervalMs) || pollingIntervalMs <= 0) {
    throw new Error("pollingIntervalMs must be a positive integer");
  }
  return clients / ((pollingIntervalMs * 0.9) / 1_000);
}
