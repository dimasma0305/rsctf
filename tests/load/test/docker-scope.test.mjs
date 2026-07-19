import assert from 'node:assert/strict';
import test from 'node:test';

import { dockerScopeFromContainerEnv, dockerWorkloadScope } from '../docker-scope.js';

test('Docker scope is replica-stable and isolates installations', () => {
  const first = dockerWorkloadScope('event-a', 'ignored-secret');
  assert.equal(first, dockerWorkloadScope('event-a', 'rotated-secret'));
  assert.notEqual(first, dockerWorkloadScope('event-b', 'ignored-secret'));
  assert.match(first, /^[a-f0-9]{32}$/);
});

test('container environment uses explicit scope before the JWT fallback', () => {
  assert.equal(
    dockerScopeFromContainerEnv([
      'RSCTF_DOCKER_SCOPE=event-a',
      'RSCTF_JWT_SECRET=secret-a',
    ]),
    dockerWorkloadScope('event-a', 'secret-b'),
  );
  assert.notEqual(
    dockerScopeFromContainerEnv(['RSCTF_JWT_SECRET=secret-a']),
    dockerScopeFromContainerEnv(['RSCTF_JWT_SECRET=secret-b']),
  );
});
