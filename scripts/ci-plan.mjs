import { execFileSync } from 'node:child_process'
import { appendFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

export const components = ['server', 'byoc', 'worker', 'web', 'docs', 'deployment', 'security']
const fullPlan = () => Object.fromEntries(components.map((name) => [name, true]))

// Only known, isolated PR paths can opt out of a gate. Unknown paths, deleted
// shared files, workflow changes, and every publication/manual run fail closed.
export function planChecks(paths, eventName) {
  if (eventName !== 'pull_request' || !paths?.length) return fullPlan()
  const plan = Object.fromEntries(components.map((name) => [name, false]))
  for (const path of paths) {
    if (path.startsWith('web/')) plan.web = true
    else if (path.startsWith('docs/')) plan.docs = true
    else if (path.startsWith('agents/byoc-agent/')) {
      for (const name of ['byoc', 'server', 'deployment', 'security']) plan[name] = true
    } else if (path.startsWith('agents/worker-agent/') || path.startsWith('lib/worker-protocol/')) {
      for (const name of ['worker', 'server', 'deployment', 'security']) plan[name] = true
    } else if (/^(README(?:\.[a-z-]+)?\.md|CHANGELOG\.md)$/.test(path)) {
      // Repository conventions and harness contracts still run for prose-only PRs.
    } else return fullPlan()
  }
  return plan
}

export const jobComponents = {
  'rust-lint': 'server',
  rust: 'server',
  coverage: 'server',
  'cheat-acceptance': 'server',
  'kubernetes-callback': 'server',
  'byoc-agent': 'byoc',
  'worker-plane-linux': 'worker',
  'worker-plane-windows': 'worker',
  web: 'web',
  docs: 'docs',
  'helm-chart': 'deployment',
  'dependency-security': 'security',
}

export function verifyChecks(plan, needs) {
  for (const name of components) {
    if (!['true', 'false'].includes(plan[name])) throw new Error(`Missing or invalid plan: ${name}`)
  }
  for (const name of ['plan', 'repository-conventions', 'load-harness-contracts']) {
    if (needs[name]?.result !== 'success') throw new Error(`Required check did not pass: ${name}`)
  }
  for (const [job, component] of Object.entries(jobComponents)) {
    const result = needs[job]?.result
    const allowed = plan[component] === 'true' ? ['success'] : ['success', 'skipped']
    if (!allowed.includes(result)) throw new Error(`Check ${job} ended with ${result ?? 'no result'}`)
  }
}

function main() {
  if (process.argv[2] === 'verify') {
    const needs = JSON.parse(process.env.CI_NEEDS)
    verifyChecks(needs.plan?.outputs ?? {}, needs)
    console.log('Every required CI check passed; only planned checks were skipped.')
    return
  }
  let paths
  if (process.env.GITHUB_EVENT_NAME === 'pull_request') {
    try {
      const base = process.env.CI_BASE_SHA
      const head = process.env.CI_HEAD_SHA
      if (![base, head].every((sha) => /^[a-f0-9]{40}$/.test(sha ?? ''))) throw new Error('invalid commit')
      // No API pagination/truncation and no shell interpolation of PR-controlled
      // filenames. --no-renames includes both sides of moves and deleted files.
      paths = execFileSync('git', ['diff', '--name-only', '-z', '--no-renames', `${base}...${head}`], {
        encoding: 'utf8', maxBuffer: 16 * 1024 * 1024,
      }).split('\0').filter(Boolean)
    } catch {
      console.log('::notice::Could not determine the complete PR diff; running all checks.')
    }
  }
  const plan = planChecks(paths, process.env.GITHUB_EVENT_NAME)
  const output = components.map((name) => `${name}=${plan[name]}\n`).join('')
  appendFileSync(process.env.GITHUB_OUTPUT, output)
  appendFileSync(process.env.GITHUB_STEP_SUMMARY, `### CI scope\n\n${components.map((name) => `- ${name}: ${plan[name] ? 'run' : 'unchanged'}`).join('\n')}\n`)
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main()
