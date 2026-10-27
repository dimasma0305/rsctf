#!/usr/bin/env node
// Validate the skill's inline local Markdown links without building the app.
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, isAbsolute, relative, resolve } from 'node:path'
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
        if (!path || isAbsolute(path) || fromRoot === '..' || fromRoot.startsWith('../') || isAbsolute(fromRoot)) {
          problems.push(`${location}: link must stay inside the checkout: ${destination}`)
        } else if (!existsSync(target)) {
          problems.push(`${location}: missing target: ${destination}`)
        }
      } catch {
        problems.push(`${location}: invalid destination: ${destination}`)
      }
    }
  }
}

if (problems.length) {
  console.error(problems.join('\n'))
  process.exitCode = 1
} else {
  console.log(`Navigation links OK: ${links} local links across ${documents.length} Markdown files.`)
}
