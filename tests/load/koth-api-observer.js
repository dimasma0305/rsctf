import { createHmac } from 'node:crypto';

function positiveId(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer`);
  }
  return parsed;
}

export function kothObservationMessage(timestamp, gameId, challengeId, rawBody) {
  const timestampText = String(timestamp);
  if (!/^\d{1,19}$/.test(timestampText)) {
    throw new Error('KotH observer timestamp must be Unix milliseconds');
  }
  if (typeof rawBody !== 'string' || Buffer.byteLength(rawBody) > 1024) {
    throw new Error('KotH observer body must be a string of at most 1024 bytes');
  }
  return `${timestampText}.${positiveId(gameId, 'gameId')}.${positiveId(challengeId, 'challengeId')}.${rawBody}`;
}

export function signKothObservation(secret, timestamp, gameId, challengeId, rawBody) {
  if (typeof secret !== 'string' || !secret.startsWith('koth_api_')) {
    throw new Error('invalid KotH observer secret');
  }
  return createHmac('sha256', secret)
    .update(kothObservationMessage(timestamp, gameId, challengeId, rawBody))
    .digest('hex');
}

export function kothObservationHeaders(secret, timestamp, gameId, challengeId, rawBody) {
  return {
    'x-rsctf-timestamp': String(timestamp),
    'x-rsctf-signature': `sha256=${signKothObservation(
      secret,
      timestamp,
      gameId,
      challengeId,
      rawBody,
    )}`,
  };
}
