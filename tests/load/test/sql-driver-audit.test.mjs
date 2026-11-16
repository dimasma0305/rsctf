import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const workflow = readFileSync(new URL('../../../.github/workflows/ci.yml', import.meta.url), 'utf8');
const step = workflow.split('      - name: Reject vulnerable optional SQL drivers in the compiled graph\n')[1]
  .split('\n      - name:')[0];
const guard = step.split('        run: |\n')[1].split('\n').map(line => line.replace(/^ {10}/, '')).join('\n');

function check(tree, cargoExit = 0) {
  const directory = mkdtempSync(join(tmpdir(), 'rsctf-sql-audit-'));
  try {
    writeFileSync(join(directory, 'tree'), tree);
    writeFileSync(join(directory, 'cargo'), '#!/bin/sh\nprintf "%s\\n" "$@" > "$RSCTF_TEST_ARGS"\ncat "$RSCTF_TEST_TREE"\nexit "$RSCTF_TEST_EXIT"\n', { mode: 0o755 });
    const result = spawnSync('bash', ['-c', guard], { encoding: 'utf8', env: {
      ...process.env, PATH: `${directory}:${process.env.PATH}`,
      RSCTF_TEST_TREE: join(directory, 'tree'), RSCTF_TEST_ARGS: join(directory, 'args'),
      RSCTF_TEST_EXIT: String(cargoExit),
    } });
    assert.equal(result.error, undefined);
    assert.deepEqual(readFileSync(join(directory, 'args'), 'utf8').trim().split('\n'),
      ['tree', '--all-features', '--target', 'all', '--locked', '--prefix', 'none']);
    return result;
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test('optional SQL audit exceptions require both drivers to be absent from every compiled target', () => {
  assert.equal(check('rsctf v0.1.175\nsqlx-postgres v0.8.6\n').status, 0);
  for (const dependency of ['rsa v0.9.10', 'libsqlite3-sys v0.30.1', 'libsqlite3-sys v0.30.1 (*)']) {
    assert.equal(check(`rsctf v0.1.175\n${dependency}\n`).status, 1);
  }
  assert.notEqual(check('', 7).status, 0, 'a failed graph query must never allow an audit exception');
});
