// Shared Docker runtime limits for load-harness containers that may be retained.
export const BOUNDED_DOCKER_LOG_ARGS = Object.freeze([
  '--log-driver',
  'json-file',
  '--log-opt',
  'max-size=5m',
  '--log-opt',
  'max-file=3',
]);
