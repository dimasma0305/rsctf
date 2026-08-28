export function validEndpointRows(rows) {
  return Array.isArray(rows) && rows.length > 0 && rows.length <= 512 && rows.every((row) =>
    row && typeof row.url === 'string' && /^wss?:\/\//.test(row.url) &&
    typeof row.token === 'string' && row.token.length > 20 && row.token.length < 8192
  );
}

export function validTrafficClose(code, reason) {
  return code === 1008 && /proxy traffic budget exceeded; retry after \d+ seconds/.test(String(reason));
}
