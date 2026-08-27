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

/** Worst-case HTTP request ceiling for the durable monitor event feed. The
 * initial visible snapshot costs one read. Each handshake/poll reconciliation
 * reads at most `maxBackfillPages`, then one checkpoint and one replacement
 * snapshot when the gap is larger than that fixed page budget. */
export function durableEventFeedRequestUpperBound({
  clients,
  durationMs,
  pollingIntervalMs,
  maxBackfillPages,
}) {
  for (const [name, value] of Object.entries({
    clients,
    durationMs,
    pollingIntervalMs,
    maxBackfillPages,
  })) {
    if (!Number.isSafeInteger(value) || value < 0)
      throw new Error(`${name} must be a non-negative integer`);
  }
  if (clients === 0 || durationMs === 0) return 0;
  if (pollingIntervalMs === 0)
    throw new Error("pollingIntervalMs must be a positive integer");
  if (maxBackfillPages === 0)
    throw new Error("maxBackfillPages must be a positive integer");

  const scheduledPolls = Math.floor(durationMs / (pollingIntervalMs * 0.9));
  const maximumRequestsPerReconciliation = maxBackfillPages + 2;
  return (
    clients *
    (1 + (1 + scheduledPolls) * maximumRequestsPerReconciliation)
  );
}
