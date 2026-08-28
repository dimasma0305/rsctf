const MAX_INCIDENT_ROWS = 100;
const MAX_INCIDENT_BODY_BYTES = 512 * 1024;
const MAX_REPORT_BODY_BYTES = 8 * 1024 * 1024;

export function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer`);
  }
  return parsed;
}

export function validIncidentPage(model, after = 0, bodyBytes = 0, ascending = true) {
  if (!model || !Array.isArray(model.incidents) || model.incidents.length > MAX_INCIDENT_ROWS) {
    return false;
  }
  if (
    !Number.isSafeInteger(model.nextCursor) ||
    model.nextCursor < after ||
    typeof model.hasMore !== 'boolean' ||
    bodyBytes > MAX_INCIDENT_BODY_BYTES
  ) {
    return false;
  }
  let cursor = ascending ? after : Number.MAX_SAFE_INTEGER;
  let maximum = after;
  const seen = new Set();
  for (const incident of model.incidents) {
    if (
      !incident ||
      !Number.isSafeInteger(incident.cursor) ||
      (ascending ? incident.cursor <= cursor : incident.cursor >= cursor) ||
      seen.has(incident.cursor) ||
      !incident.ownedTeam ||
      !incident.submitTeam ||
      !incident.submission
    ) {
      return false;
    }
    seen.add(incident.cursor);
    cursor = incident.cursor;
    maximum = Math.max(maximum, incident.cursor);
  }
  return model.nextCursor === (model.incidents.length ? maximum : after);
}

export function validConditionalReport(status, bodyBytes, etag, retryAfter) {
  if (status === 304) return bodyBytes === 0 && /^W\/"rsctf-cheat-report-[a-f0-9]{64}"$/.test(etag);
  if (status === 503) return /^[1-9]\d*$/.test(retryAfter);
  return status === 200 && bodyBytes <= MAX_REPORT_BODY_BYTES && /^W\/"rsctf-cheat-report-[a-f0-9]{64}"$/.test(etag);
}

export function ledgerFingerprint(row) {
  const values = String(row || '').split('|');
  if (values.length !== 6 || values.some((value) => !/^\d+$/.test(value))) {
    throw new Error('anti-cheat ledger fingerprint is malformed');
  }
  return values.join('|');
}
