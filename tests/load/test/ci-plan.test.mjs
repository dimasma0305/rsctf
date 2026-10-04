import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'
import { components, jobComponents, planChecks, verifyChecks } from '../../../scripts/ci-plan.mjs'

const all = Object.fromEntries(components.map((name) => [name, true]))
test('publication, manual, unknown and indeterminate changes always run the full suite', () => {
  for (const event of ['workflow_dispatch', 'workflow_call', 'push', undefined]) {
    assert.deepEqual(planChecks(['web/package.json'], event), all)
    assert.deepEqual(planChecks(['.github/WORKFLOW_PERFORMANCE.md', 'tests/load/REPORT.md'], event), all)
  }
  for (const paths of [undefined, [], ['.github/workflows/ci.yml'], ['Cargo.lock'], ['build.rs'], ['src/new.rs'], ['scripts/ci-plan.mjs'], ['new-shared-config']]) {
    assert.deepEqual(planChecks(paths, 'pull_request'), all)
  }
})

test('only the exact non-build report paths omit unrelated component jobs on PRs', () => {
  const reports = ['.github/WORKFLOW_PERFORMANCE.md', 'tests/load/README.md', 'tests/load/REPORT.md']
  const none = Object.fromEntries(components.map((name) => [name, false]))
  for (const paths of reports.map((path) => [path]).concat([reports])) {
    assert.deepEqual(planChecks(paths, 'pull_request'), none)
  }
  for (const path of ['.github/WORKFLOW_PERFORMANCE.md.yml', 'tests/load/REPORT.md.js',
    '.github/workflows/ci.yml', 'tests/load/k6/polled-read.js', 'tests/load/test/ci-plan.test.mjs',
    'scripts/ci-plan.mjs', 'LICENSING.md', 'LICENSE.txt', 'build.rs']) {
    assert.deepEqual(planChecks([...reports, path], 'pull_request'), all)
  }
  const web = planChecks([...reports, 'web/package.json'], 'pull_request')
  assert.deepEqual(Object.keys(web).filter((name) => web[name]), ['web'])
})

test('leaf PRs select their checks without skipping cross-component Rust contracts', () => {
  const web = planChecks(['web/package.json', 'web/pnpm-lock.yaml'], 'pull_request')
  assert.deepEqual(Object.keys(web).filter((key) => web[key]), ['web'])
  const docs = planChecks(['docs/index.md', 'README.md'], 'pull_request')
  assert.deepEqual(Object.keys(docs).filter((key) => docs[key]), ['docs'])
  for (const path of ['agents/worker-agent/Cargo.lock', 'lib/worker-protocol/src/lib.rs']) {
    const plan = planChecks([path], 'pull_request')
    for (const name of ['worker', 'server', 'deployment', 'security']) assert.equal(plan[name], true)
  }
  assert.equal(planChecks(['agents/byoc-agent/Cargo.lock'], 'pull_request').server, true)
})

test('deleted/moved paths and mixed changes are cumulative, not just the final path', () => {
  assert.deepEqual(planChecks(['web/new.json', 'Cargo.toml'], 'pull_request'), all)
  assert.deepEqual(planChecks(['web/a; echo ignored', '.github/actions/setup/action.yml'], 'pull_request'), all)
  const mixed = planChecks(['web/a', 'docs/b'], 'pull_request')
  assert.equal(mixed.web, true)
  assert.equal(mixed.docs, true)
})

function results(plan) {
  return Object.fromEntries([
    ...['plan', 'repository-conventions', 'load-harness-contracts'].map((name) => [name, { result: 'success' }]),
    ...Object.entries(jobComponents).map(([job, component]) => [job, { result: plan[component] === 'true' ? 'success' : 'skipped' }]),
  ])
}

test('report-only PRs retain every unconditional check and reject unsuccessful optional jobs', () => {
  const plan = Object.fromEntries(Object.entries(planChecks(['tests/load/REPORT.md'], 'pull_request'))
    .map(([name, selected]) => [name, String(selected)]))
  assert.doesNotThrow(() => verifyChecks(plan, results(plan)))
  for (const job of ['plan', 'repository-conventions', 'load-harness-contracts']) {
    for (const result of ['failure', 'cancelled', 'skipped', undefined]) {
      assert.throws(() => verifyChecks(plan, { ...results(plan), [job]: { result } }))
    }
  }
  for (const job of Object.keys(jobComponents)) {
    for (const result of ['failure', 'cancelled', undefined]) {
      assert.throws(() => verifyChecks(plan, { ...results(plan), [job]: { result } }))
    }
  }
})

test('the aggregate gate accepts planned skips and rejects missing, failed, cancelled or unexpectedly skipped checks', () => {
  const plan = Object.fromEntries(Object.entries(planChecks(['web/a'], 'pull_request')).map(([key, value]) => [key, String(value)]))
  assert.doesNotThrow(() => verifyChecks(plan, results(plan)))
  for (const job of ['plan', 'repository-conventions', 'load-harness-contracts', 'web']) {
    for (const result of ['failure', 'cancelled', 'skipped', undefined]) {
      assert.throws(() => verifyChecks(plan, { ...results(plan), [job]: { result } }))
    }
  }
  assert.throws(() => verifyChecks({ ...plan, server: undefined }, results(plan)))
  assert.throws(() => verifyChecks(plan, { ...results(plan), rust: { result: 'failure' } }))
  const full = Object.fromEntries(components.map((name) => [name, 'true']))
  for (const job of Object.keys(jobComponents)) {
    for (const result of ['failure', 'cancelled', 'skipped', undefined]) {
      assert.throws(() => verifyChecks(full, { ...results(full), [job]: { result } }))
    }
  }
})

test('the CLI handles complete git diffs, path moves, hostile filenames and unavailable history', () => {
  const root = mkdtempSync(join(tmpdir(), 'rsctf-ci-plan-'))
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim()
  const script = fileURLToPath(new URL('../../../scripts/ci-plan.mjs', import.meta.url))
  const output = join(root, '.git', 'ci-output')
  const run = (base, head) => {
    writeFileSync(output, '')
    execFileSync(process.execPath, [script], {
      cwd: root,
      env: { ...process.env, GITHUB_EVENT_NAME: 'pull_request', CI_BASE_SHA: base, CI_HEAD_SHA: head, GITHUB_OUTPUT: output, GITHUB_STEP_SUMMARY: join(root, '.git', 'ci-summary') },
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    return Object.fromEntries(readFileSync(output, 'utf8').trim().split('\n').map((line) => line.split('=')))
  }
  try {
    git('init')
    git('config', 'user.name', 'CI fixture')
    git('config', 'user.email', 'fixture@example.invalid')
    writeFileSync(join(root, 'Cargo.toml'), 'fixture\n')
    git('add', '.')
    git('commit', '-m', 'base')
    const base = git('rev-parse', 'HEAD')
    mkdirSync(join(root, 'web'))
    for (let index = 0; index < 350; index++) writeFileSync(join(root, 'web', `${index}.txt`), 'fixture\n')
    writeFileSync(join(root, 'web', 'space and\nnewline;$(false).txt'), 'fixture\n')
    git('add', '.')
    git('commit', '-m', 'web')
    const web = git('rev-parse', 'HEAD')
    assert.equal(run(base, web).server, 'false')
    assert.equal(run(base, web).web, 'true')
    git('mv', 'Cargo.toml', 'web/moved.toml')
    git('commit', '-m', 'move shared input')
    assert.equal(run(base, git('rev-parse', 'HEAD')).server, 'true')
    assert.equal(run('0'.repeat(40), web).server, 'true')
    assert.equal(run('invalid', web).server, 'true')
    const beforeReport = git('rev-parse', 'HEAD')
    mkdirSync(join(root, '.github'))
    writeFileSync(join(root, '.github', 'WORKFLOW_PERFORMANCE.md'), 'Measured results.\n')
    git('add', '.')
    git('commit', '-m', 'report only')
    const report = git('rev-parse', 'HEAD')
    assert.ok(Object.values(run(beforeReport, report)).every((selected) => selected === 'false'))
    mkdirSync(join(root, '.github', 'workflows'))
    writeFileSync(join(root, '.github', 'workflows', 'ci.yml'), 'fixture\n')
    git('add', '.')
    git('commit', '-m', 'report plus workflow')
    assert.ok(Object.values(run(beforeReport, git('rev-parse', 'HEAD'))).every((selected) => selected === 'true'))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
