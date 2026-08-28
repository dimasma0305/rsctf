import { createHash } from 'node:crypto';

import { requireAdToken } from './ad-bearer-admission.js';

export function tokenHash(token) {
  return createHash('sha256').update(requireAdToken(token, 'token')).digest('hex');
}
