#!/usr/bin/env node
// Validate the skill's inline local Markdown links without building the app.
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

const skillRoot = fileURLToPath(new URL('../', import.meta.url))
const repositoryRoot = resolve(skillRoot, '../../..')

function markdownFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = resolve(directory, entry.name)
    return entry.isDirectory() ? markdownFiles(path) : entry.name.endsWith('.md') ? [path] : []
  })
}

let links = 0
const problems = []
const mapTargets = new Set()
const documents = [...markdownFiles(skillRoot), resolve(repositoryRoot, 'AGENTS.md')]
for (const file of documents) {
  // Skip fenced examples; a shell example may contain Markdown-like syntax.
  let fence
  const lines = readFileSync(file, 'utf8').split('\n')
  for (const [index, line] of lines.entries()) {
    const marker = line.match(/^\s*(`{3,}|~{3,})/)?.[1]
    if (marker) {
      if (!fence) fence = marker
      else if (marker[0] === fence[0] && marker.length >= fence.length) fence = undefined
      continue
    }
    if (fence) continue
    // Map links use inline destinations, including labels with nested [id].
    for (const match of line.matchAll(/\]\(([^\n)]+)\)/g)) {
      const destination = match[1].trim()
      if (/^(?:https?:|mailto:|#)/i.test(destination)) continue
      links++
      const location = `${relative(repositoryRoot, file)}:${index + 1}`
      try {
        const path = decodeURIComponent(destination.split(/[?#]/, 1)[0])
        const target = resolve(dirname(file), path)
        const fromRoot = relative(repositoryRoot, target)
        if (!path || isAbsolute(path) || fromRoot === '..' || fromRoot.startsWith(`..${sep}`) || isAbsolute(fromRoot)) {
          problems.push(`${location}: link must stay inside the checkout: ${destination}`)
        } else if (!existsSync(target)) {
          problems.push(`${location}: missing target: ${destination}`)
        } else if (file.endsWith('-map.md')) {
          mapTargets.add(fromRoot.split(sep).join('/').replace(/\/$/, ''))
        }
      } catch {
        problems.push(`${location}: invalid destination: ${destination}`)
      }
    }
  }
}

// Check family entry points, not every function, endpoint or permission boundary.
// Broad src/ and pages/ directory links do not count as coverage of their children.
const coverage = []
for (const group of ['controllers', 'services', 'hubs', 'storage']) {
  const source = readFileSync(resolve(repositoryRoot, `src/${group}/mod.rs`), 'utf8')
  const modules = [...source.matchAll(/^pub(?:\(crate\))? mod (\w+);/gm)].map(match => match[1])
  let covered = 0
  for (const name of modules) {
    const base = `src/${group}/${name}`
    if ([...mapTargets].some(path => path === base || path === `${base}.rs` || path.startsWith(`${base}/`))) covered++
    else problems.push(`Unmapped ${group} module: ${base}; add an owner link to a layer map`)
  }
  coverage.push(`${group} ${covered}/${modules.length}`)
}

function pageFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = resolve(directory, entry.name)
    return entry.isDirectory() ? pageFiles(path) : entry.name.endsWith('.tsx') ? [path] : []
  })
}
const pages = pageFiles(resolve(repositoryRoot, 'web/src/pages'))
let coveredPages = 0
for (const page of pages) {
  const path = relative(repositoryRoot, page).split(sep).join('/')
  if ([...mapTargets].some(target => target === path ||
      (target !== 'web/src/pages' && path.startsWith(`${target}/`)))) coveredPages++
  else problems.push(`Unmapped page: ${path}; add its file or owning page-family link`)
}
coverage.push(`page families ${coveredPages}/${pages.length}`)

if (problems.length) {
  console.error(problems.join('\n'))
  process.exitCode = 1
} else {
  console.log(`Navigation links OK: ${links} local links across ${documents.length} Markdown files.`)
  console.log(`Navigation coverage: ${coverage.join('; ')}.`)
}
