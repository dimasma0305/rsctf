import assert from 'node:assert/strict';
import test from 'node:test';

import { countContainerFatalLogs, fatalLogLineCount } from '../log-audit.mjs';

test('counts fatal records across both Docker log streams', () => {
  assert.equal(
    fatalLogLineCount('ordinary info\nthread panicked at src/main.rs', 'FATAL: database unavailable\nhealthy'),
    2,
  );
});

test('audits only the configured container and current run window', () => {
  const calls = [];
  const count = countContainerFatalLogs('rsctf-isolated-1', 1_784_466_640_229, (command, args, options) => {
    calls.push({ command, args, options });
    return { status: 0, stdout: 'healthy\n', stderr: '' };
  });

  assert.equal(count, 0);
  assert.deepEqual(calls[0].args, [
    'logs',
    '--since',
    '2026-07-19T13:10:40.229Z',
    'rsctf-isolated-1',
  ]);
  assert.equal(calls[0].command, 'docker');
  assert.equal(calls[0].options.encoding, 'utf8');
});

test('fails closed when Docker cannot provide the selected log window', () => {
  assert.throws(
    () =>
      countContainerFatalLogs('rsctf-isolated-1', 1_784_466_640_229, () => ({
        status: 1,
        stdout: '',
        stderr: 'no such container',
      })),
    /could not audit rsctf-isolated-1 logs: no such container/,
  );
});
