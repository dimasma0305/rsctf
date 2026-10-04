function validEndpointRow(row) {
  if (!row || typeof row.url !== 'string') return false;
  if (row.url.length > 2048) return false;
  const match = /^(wss?):\/\/([^/?#\s]+)([^#\s]*)$/.exec(row.url);
  if (!match || match[2].includes('@')) return false;

  const bearerToken = row.bearerToken;
  const sessionCookie = row.sessionCookie;
  if (bearerToken !== undefined &&
      (typeof bearerToken !== 'string' || bearerToken.length <= 20 || bearerToken.length >= 8192)) {
    return false;
  }
  if (sessionCookie !== undefined &&
      (typeof sessionCookie !== 'string' ||
       !/^RSCTF_Token=[^;\r\n]{21,8191}$/.test(sessionCookie))) {
    return false;
  }

  const hasCapability = /(?:[?&])capability=[^&#\s]+/.test(row.url);
  return hasCapability || bearerToken !== undefined || sessionCookie !== undefined;
}

export function validEndpointRows(rows) {
  if (!Array.isArray(rows) || rows.length === 0 || rows.length > 512 ||
      !rows.every(validEndpointRow)) {
    return false;
  }
  return new Set(rows.map((row) => [
    row.url,
    row.bearerToken || '',
    row.sessionCookie || '',
  ].join('\u0000'))).size === rows.length;
}

export function durationMilliseconds(value) {
  const match = /^([1-9]\d*)(ms|s|m|h)$/.exec(String(value));
  if (!match) return null;
  const amount = Number(match[1]);
  const factor = { ms: 1, s: 1_000, m: 60_000, h: 3_600_000 }[match[2]];
  const duration = amount * factor;
  return Number.isSafeInteger(duration) ? duration : null;
}

export function endpointOriginMatchesTarget(endpointUrl, target) {
  try {
    const endpoint = new URL(endpointUrl);
    const targetUrl = new URL(target);
    const endpointProtocol = endpoint.protocol === 'wss:' ? 'https:' : 'http:';
    return `${endpointProtocol}//${endpoint.host}` === targetUrl.origin;
  } catch {
    return false;
  }
}

export function validTrafficClose(code, reason) {
  return code === 1008 &&
    /^proxy traffic budget exceeded; retry after \d+ seconds$/.test(String(reason));
}

export function validAdmissionRejection(status, retryAfter) {
  const normalized = Array.isArray(retryAfter) ? retryAfter[0] : retryAfter;
  return status === 429 && /^\d+$/.test(String(normalized || '').trim()) &&
    Number.parseInt(String(normalized), 10) > 0;
}
