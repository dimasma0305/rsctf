export const AD_TOKEN_PATTERN = /^ad_[A-Za-z0-9_-]{43}$/;

export function requireAdToken(value, label) {
  if (!AD_TOKEN_PATTERN.test(String(value || ''))) throw new Error(`${label} must be one fixed-shape A&D bearer token`);
  return String(value);
}

export function validTargetModel(model) {
  return Boolean(
    model &&
    Number.isSafeInteger(model.currentRound) &&
    model.currentRound >= 0 &&
    Array.isArray(model.challenges) &&
    model.challenges.every((challenge) =>
      Number.isSafeInteger(challenge?.challengeId) &&
      typeof challenge.title === 'string' &&
      Array.isArray(challenge.teams),
    ),
  );
}

export function expectedBearerStatus(kind, status, retryAfter = '') {
  if (status === 429) return /^[1-9]\d*$/.test(retryAfter);
  if (kind === 'valid' || kind === 'rotated') return status === 200;
  if (kind === 'slow') return status === 503;
  return status === 401 || status === 403;
}
