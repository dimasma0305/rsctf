function responseStatus(value) {
  const status = Number(value?.status ?? value);
  if (!Number.isSafeInteger(status) || status < 100 || status > 599) {
    throw new Error(`token readiness probe returned an invalid HTTP status (${status})`);
  }
  return status;
}

export async function ensureValidTeamToken({
  token,
  probe,
  rotate,
  wait = async () => {},
  maxAttempts = 20,
}) {
  if (typeof token !== 'string' || !token) throw new Error('token readiness requires a token');
  if (typeof probe !== 'function' || typeof rotate !== 'function' || typeof wait !== 'function') {
    throw new TypeError('token readiness callbacks must be functions');
  }
  if (!Number.isSafeInteger(maxAttempts) || maxAttempts < 1 || maxAttempts > 100) {
    throw new Error(`token readiness attempts must be in 1..100 (got ${maxAttempts})`);
  }

  let candidate = token;
  let rotated = false;
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    const response = await probe(candidate);
    const status = responseStatus(response);
    if (status === 200) return candidate;
    if (status === 401 || status === 403) {
      if (rotated || attempt + 1 >= maxAttempts) {
        throw new Error('token readiness remained unauthorized after bounded repair');
      }
      const replacement = await rotate();
      if (typeof replacement !== 'string' || !replacement || replacement === candidate) {
        throw new Error('token rotation returned an invalid credential');
      }
      candidate = replacement;
      rotated = true;
      continue;
    }
    if (status === 429 || status >= 500) {
      await wait(Math.min(5_000, 500 * (attempt + 1)));
      continue;
    }
    throw new Error(`token readiness probe was rejected with HTTP ${status}`);
  }
  throw new Error('token readiness could not be established after bounded retries');
}
