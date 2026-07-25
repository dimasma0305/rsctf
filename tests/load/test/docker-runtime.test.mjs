import assert from 'node:assert/strict';
import test from 'node:test';

import { BOUNDED_DOCKER_LOG_ARGS } from '../docker-runtime.js';

test('retained load containers use bounded Docker JSON logs', () => {
  assert.deepEqual(BOUNDED_DOCKER_LOG_ARGS, [
    '--log-driver',
    'json-file',
    '--log-opt',
    'max-size=5m',
    '--log-opt',
    'max-file=3',
  ]);
  assert.equal(Object.isFrozen(BOUNDED_DOCKER_LOG_ARGS), true);
});
