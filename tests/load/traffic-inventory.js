export const MAX_TRAFFIC_ROWS = 100;
export const MAX_TRAFFIC_BODY_BYTES = 512 * 1024;

export function validTrafficRows(rows, kind, maxRows = MAX_TRAFFIC_ROWS, bodyBytes = 0) {
  if (!Array.isArray(rows) || rows.length > maxRows || bodyBytes > MAX_TRAFFIC_BODY_BYTES) return false;
  const identities = new Set();
  return rows.every((row) => {
    if (!row || typeof row !== 'object') return false;
    let identity;
    let valid;
    if (kind === 'games') {
      identity = row.id;
      valid = Number.isSafeInteger(row.id) && typeof row.title === 'string' && Number.isSafeInteger(row.count) && row.count >= 0;
    } else if (kind === 'teams') {
      identity = row.id;
      valid = Number.isSafeInteger(row.id) && Number.isSafeInteger(row.teamId) && typeof row.name === 'string' && Number.isSafeInteger(row.count) && row.count > 0;
    } else {
      identity = row.fileName;
      valid = typeof row.fileName === 'string' && row.fileName.toLowerCase().endsWith('.pcap') && Number.isFinite(row.size) && row.size >= 0 && Number.isFinite(row.updateTime);
    }
    if (!valid || identities.has(identity)) return false;
    identities.add(identity);
    return true;
  });
}

export function captureFingerprint(rows) {
  if (!Array.isArray(rows)) throw new Error('capture fingerprint requires rows');
  return rows
    .map((row) => `${row.id ?? row.fileName}:${row.count ?? row.size}`)
    .sort()
    .join('|');
}
