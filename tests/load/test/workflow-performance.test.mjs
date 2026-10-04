import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { jobComponents } from '../../../scripts/ci-plan.mjs'

const dockerfile = readFileSync(new URL('../../../Dockerfile', import.meta.url), 'utf8')
const agentImage = readFileSync(
  new URL('../../../src/controllers/game/ad/byoc/agent_image.rs', import.meta.url),
  'utf8',
)
const ciWorkflow = readFileSync(new URL('../../../.github/workflows/ci.yml', import.meta.url), 'utf8')
const imageWorkflow = readFileSync(new URL('../../../.github/workflows/image.yml', import.meta.url), 'utf8')
const releaseWorkflow = readFileSync(
  new URL('../../../.github/workflows/worker-agent-release.yml', import.meta.url),
  'utf8',
)

test('the companion digest cannot invalidate Rust release compilation', () => {
  const cook = dockerfile.indexOf('cargo chef cook --release --locked --recipe-path recipe.json')
  const runtime = dockerfile.indexOf('FROM debian:bookworm-slim')
  assert.ok(cook >= 0 && runtime > cook)
  assert.doesNotMatch(dockerfile.slice(cook, runtime), /RSCTF_DEFAULT_BYOC_AGENT/)
  assert.match(
    dockerfile.slice(runtime),
    /ARG RSCTF_DEFAULT_BYOC_AGENT_IMAGE[\s\S]*ENV RSCTF_DEFAULT_BYOC_AGENT_IMAGE=/,
  )
  assert.match(agentImage, /std::env::var\("RSCTF_DEFAULT_BYOC_AGENT_IMAGE"\)/)
  assert.doesNotMatch(agentImage, /option_env!\("RSCTF_DEFAULT_BYOC_AGENT_IMAGE"\)/)
})

test('manual and tag publication reuse one attested quality decision', () => {
  const triggers = ciWorkflow.slice(0, ciWorkflow.indexOf('permissions:'))
  assert.doesNotMatch(triggers, /^  push:\s*$/m)
  assert.match(triggers, /^  pull_request:\s*$/m)
  assert.match(triggers, /^  workflow_call:\s*$/m)

  const imageTriggers = imageWorkflow.slice(0, imageWorkflow.indexOf('permissions:'))
  assert.match(imageTriggers, /^  workflow_dispatch:\s*$/m)
  assert.doesNotMatch(imageTriggers, /^    branches:/m)
  assert.doesNotMatch(imageTriggers, /^  pull_request:\s*$/m)
  assert.match(
    imageWorkflow,
    /quality:[\s\S]*uses: \.\/\.github\/workflows\/ci\.yml[\s\S]*finalize-main:/,
  )
  assert.match(imageWorkflow, /source-ref refs\/heads\/main/)
  assert.doesNotMatch(releaseWorkflow, /^  verify:\s*$/m)
  assert.match(
    releaseWorkflow,
    /publish:[\s\S]*needs: \[build-linux, build-windows\][\s\S]*gh attestation verify/,
  )
})

function job(source, name) {
  const match = source.match(new RegExp(`^  ${name}:\\n([\\s\\S]*?)(?=^  [a-z][a-z-]*:|$(?![\\s\\S]))`, 'm'))
  assert.ok(match, `missing ${name} job`)
  return match[1]
}

test('every scoped check is selected by the planner and enforced by the always-running aggregate', () => {
  const required = job(ciWorkflow, 'required')
  assert.match(required, /if: always\(\)/)
  assert.match(required, /node scripts\/ci-plan\.mjs verify/)
  for (const [name, component] of Object.entries(jobComponents)) {
    assert.match(job(ciWorkflow, name), new RegExp(`if: needs\\.plan\\.outputs\\.${component} == 'true'`))
    assert.match(required.split('runs-on:')[0], new RegExp(`\\b${name}\\b`))
  }
  assert.match(job(ciWorkflow, 'plan'), /fetch-depth: 0/)
  assert.match(job(ciWorkflow, 'plan'), /CI_BASE_SHA:.*pull_request\.base\.sha/)
  assert.match(job(ciWorkflow, 'plan'), /CI_HEAD_SHA:.*pull_request\.head\.sha/)
})

test('integration jobs use the exact same-run test outputs instead of compiling a third server', () => {
  const rust = job(ciWorkflow, 'rust')
  assert.match(rust, /cargo test --all-targets --all-features --locked/)
  assert.match(rust, /cargo clippy --all-targets --all-features --locked -- -D warnings/)
  assert.doesNotMatch(rust, /cargo check/)
  assert.match(rust, /name: ci-server-\$\{\{ github\.sha }}/)
  assert.match(rust, /retention-days: 1/)
  for (const name of ['kubernetes-callback', 'cheat-acceptance']) {
    const source = job(ciWorkflow, name)
    assert.match(source, /needs: \[plan, rust\]/)
    assert.match(source, /name: ci-server-\$\{\{ github\.sha }}/)
    assert.match(source, /source-sha\)" = "\$GITHUB_SHA"/)
    assert.doesNotMatch(source, /cargo (build|test)|rust-cache|rust-toolchain/)
  }
})

test('documentation has one reusable build and no duplicate PR trigger', () => {
  const docs = readFileSync(new URL('../../../.github/workflows/docs.yml', import.meta.url), 'utf8')
  const shared = readFileSync(new URL('../../../.github/workflows/docs-build.yml', import.meta.url), 'utf8')
  assert.doesNotMatch(docs, /^  pull_request:/m)
  assert.match(docs, /uses: \.\/\.github\/workflows\/docs-build\.yml/)
  assert.match(job(ciWorkflow, 'docs'), /uses: \.\/\.github\/workflows\/docs-build\.yml/)
  assert.match(shared, /pnpm --dir docs pdf/)
  assert.match(shared, /pnpm --dir docs build/)
  assert.match(shared, /if: inputs\.pages && github\.ref == 'refs\/heads\/main'/)
})

test('worker release restores main caches and does not export unusable per-tag copies', () => {
  assert.match(job(ciWorkflow, 'worker-plane-linux'), /shared-key: worker-plane-linux/)
  assert.match(job(ciWorkflow, 'worker-plane-windows'), /shared-key: worker-plane-windows/)
  for (const name of ['build-linux', 'build-windows']) {
    const source = job(releaseWorkflow, name)
    assert.match(source, /lib\/worker-protocol -> target\n\s+agents\/worker-agent -> target/)
    assert.match(source, /save-if: false/)
    assert.match(source, /cargo build[\s\S]*--release --target[\s\S]*--locked/)
  }
  assert.match(job(releaseWorkflow, 'build-linux'), /'worker-plane-linux'/)
  assert.match(job(releaseWorkflow, 'build-windows'), /shared-key: worker-plane-windows/)
})
