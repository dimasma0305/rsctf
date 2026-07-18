import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const SHA256_HEX = /^[0-9a-f]{64}$/i;

function sha256(value) {
  return createHash('sha256').update(value).digest('hex');
}

function git(repositoryRoot, args) {
  return execFileSync('git', args, {
    cwd: repositoryRoot,
    encoding: null,
    env: process.env,
  });
}

function normalize(value, label) {
  const normalized = String(value || '').trim().toLowerCase();
  if (!SHA256_HEX.test(normalized)) {
    throw new Error(`${label} must be a 64-character SHA-256 fingerprint`);
  }
  return normalized;
}

function reproducibleSubmoduleStatus(root) {
  const status = git(root, ['submodule', 'status', '--recursive']);
  const invalid = status
    .toString('utf8')
    .split('\n')
    .filter(Boolean)
    .find((line) => line[0] !== ' ');
  if (invalid) {
    throw new Error(
      `submodules must be initialized at their pinned commits before fingerprinting: ${invalid}`,
    );
  }
  const dirty = git(root, [
    'submodule',
    'foreach',
    '--quiet',
    '--recursive',
    'if ! git diff HEAD --quiet -- || test -n "$(git ls-files --others --exclude-standard)"; then printf "%s\\n" "$displaypath"; fi; true',
  ])
    .toString('utf8')
    .trim();
  if (dirty) {
    throw new Error(
      `submodules must have no staged, unstaged, or untracked changes before fingerprinting: ${dirty}`,
    );
  }
  return status;
}

export function expectedSourceFingerprints(environment) {
  const tracked = String(environment.E2E_EXPECTED_TRACKED_SHA256 || '').trim();
  const untracked = String(environment.E2E_EXPECTED_UNTRACKED_SHA256 || '').trim();
  if (!tracked || !untracked) {
    throw new Error(
      'E2E_EXPECTED_TRACKED_SHA256 and E2E_EXPECTED_UNTRACKED_SHA256 are both required',
    );
  }
  return {
    tracked: normalize(tracked, 'E2E_EXPECTED_TRACKED_SHA256'),
    untracked: normalize(untracked, 'E2E_EXPECTED_UNTRACKED_SHA256'),
  };
}

export function repositorySourceFingerprints(repositoryRoot) {
  const root = resolve(repositoryRoot);
  const submodules = reproducibleSubmoduleStatus(root);
  const tracked = sha256(Buffer.concat([
    Buffer.from('HEAD\n'),
    git(root, ['rev-parse', '--verify', 'HEAD']),
    Buffer.from('DIFF_HEAD_BINARY\n'),
    git(root, ['diff', 'HEAD', '--binary']),
    Buffer.from('SUBMODULE_STATUS\n'),
    submodules,
  ]));
  const listed = git(root, ['ls-files', '--others', '--exclude-standard', '-z'])
    .toString('utf8')
    .split('\0')
    .filter(Boolean)
    .sort((left, right) => Buffer.compare(Buffer.from(left), Buffer.from(right)));

  let manifest;
  if (listed.length === 0) {
    // Match GNU xargs, which invokes sha256sum once with empty stdin when it
    // receives no paths in the documented reproduction pipeline.
    manifest = `${sha256(Buffer.alloc(0))}  -\n`;
  } else {
    manifest = listed
      .map((path) => {
        if (/[\\\r\n]/.test(path)) {
          throw new Error(`untracked path cannot be fingerprinted safely: ${JSON.stringify(path)}`);
        }
        const absolute = resolve(root, path);
        if (!absolute.startsWith(`${root}/`)) {
          throw new Error(`untracked path escapes the repository: ${JSON.stringify(path)}`);
        }
        return `${sha256(readFileSync(absolute))}  ${path}\n`;
      })
      .join('');
  }

  return { tracked, untracked: sha256(manifest) };
}

export function assertSourceFingerprints(expected, actual, stage) {
  const mismatches = ['tracked', 'untracked'].filter(
    (field) => expected[field] !== actual[field],
  );
  if (mismatches.length) {
    throw new Error(
      `${stage} source fingerprint changed (${mismatches.join(', ')}): ` +
        `expected ${expected.tracked}/${expected.untracked}, ` +
        `got ${actual.tracked}/${actual.untracked}`,
    );
  }
  return actual;
}

export function assertRepositorySourceFingerprints(repositoryRoot, expected, stage) {
  return assertSourceFingerprints(
    expected,
    repositorySourceFingerprints(repositoryRoot),
    stage,
  );
}
