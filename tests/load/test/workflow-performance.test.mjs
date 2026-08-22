import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

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

test('main and tag publication reuse one attested quality decision', () => {
  const triggers = ciWorkflow.slice(0, ciWorkflow.indexOf('permissions:'))
  assert.doesNotMatch(triggers, /^  push:\s*$/m)
  assert.match(triggers, /^  pull_request:\s*$/m)
  assert.match(triggers, /^  workflow_call:\s*$/m)

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
