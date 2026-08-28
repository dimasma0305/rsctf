export function validWorkerInventory(rows, workerId, expectedOnline) {
  if (!Array.isArray(rows)) return false;
  const worker = rows.find((row) => String(row?.id || '').toLowerCase() === workerId.toLowerCase());
  return Boolean(
    worker &&
    worker.online === expectedOnline &&
    typeof worker.sessionEpoch === 'number' &&
    worker.capacity &&
    Number.isFinite(worker.capacity.slots),
  );
}

export function isBoundedImageFailure(status, body) {
  return status === 503 && /image|pull|workload|worker/i.test(String(body || ''));
}

export function validHealthyProxyEndpoints(rows, outageWorkerId) {
  if (!Array.isArray(rows) || rows.length < 2) return false;
  const kinds = new Set();
  for (const row of rows) {
    if (
      !row ||
      !['player', 'checker'].includes(row.kind) ||
      String(row.workerId || '').toLowerCase() === outageWorkerId.toLowerCase() ||
      !/^wss?:\/\//.test(String(row.url || '')) ||
      typeof row.token !== 'string' ||
      row.token.length < 16
    ) return false;
    kinds.add(row.kind);
  }
  return kinds.has('player') && kinds.has('checker');
}
