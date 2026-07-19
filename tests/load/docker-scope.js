import { createHash } from 'node:crypto';

export function dockerWorkloadScope(explicit, jwtSecret) {
  const configured = String(explicit || '').trim();
  const secret = String(jwtSecret || '').trim();
  const source = configured ? 'explicit' : secret ? 'jwt' : 'development';
  const identity = configured || secret || 'rsctf';
  return createHash('sha256').update(`${source}\0${identity}`).digest('hex').slice(0, 32);
}

export function dockerScopeFromContainerEnv(environment) {
  const values = new Map(
    (environment || []).map((entry) => {
      const separator = String(entry).indexOf('=');
      return separator < 0
        ? [String(entry), '']
        : [String(entry).slice(0, separator), String(entry).slice(separator + 1)];
    }),
  );
  return dockerWorkloadScope(values.get('RSCTF_DOCKER_SCOPE'), values.get('RSCTF_JWT_SECRET'));
}
