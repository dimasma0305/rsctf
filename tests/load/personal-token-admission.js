const PERSONAL_TOKEN = /^rsctf_pat_v1_[A-Za-z0-9_-]{43}$/;

export function requirePersonalToken(value, label) {
  const token = String(value || '');
  if (!PERSONAL_TOKEN.test(token)) throw new Error(`${label} must be a versioned rsctf personal token`);
  return token;
}

export function expectedPersonalTokenStatus(kind, status, retryAfter = '') {
  if (status === 429) return /^\d+$/.test(String(retryAfter)) && Number(retryAfter) >= 1;
  if (kind === 'valid') return status === 200;
  return status === 401;
}

export function validTokenPage(value) {
  return Boolean(
    value && Array.isArray(value.data) && Number.isSafeInteger(value.total) &&
      Number.isSafeInteger(value.length) && value.length === value.data.length && value.data.length <= 1,
  );
}
