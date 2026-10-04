import crypto from 'k6/crypto';

/** Stable RFC-4122-shaped opaque ID for one load-model semantic submit. */
export function submitAttemptId(seed) {
  const hex = crypto.sha256(`rsctf-submit-attempt-v1:${seed}`, 'hex');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-4${hex.slice(13, 16)}-a${hex.slice(17, 20)}-${hex.slice(20, 32)}`;
}
