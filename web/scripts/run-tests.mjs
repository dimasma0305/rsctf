// Minimal test runner for the ClientApp: finds every `*.test.ts` under src/,
// transpiles each with esbuild (already a dependency — no test framework added),
// and runs them through Node's built-in test runner (node:test). Pure-logic unit
// tests only (no DOM); exits non-zero if any test fails.
import { build } from 'esbuild'
import { mkdtempSync, readdirSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

function findTests(dir, acc = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name)
    if (statSync(p).isDirectory()) findTests(p, acc)
    else if (name.endsWith('.test.ts')) acc.push(p)
  }
  return acc
}

const entries = findTests('src')
if (entries.length === 0) {
  console.log('No *.test.ts files found under src/.')
  process.exit(0)
}

const outDir = mkdtempSync(join(tmpdir(), 'rsctf-web-test-'))
const outFiles = []
let i = 0
for (const entry of entries) {
  const outFile = join(outDir, `test-${i++}.mjs`)
  await build({ entryPoints: [entry], outfile: outFile, bundle: true, platform: 'node', format: 'esm' })
  outFiles.push(outFile)
}

// Importing a bundled module registers its node:test cases; the runner executes
// them at process exit and sets a non-zero exit code on any failure.
for (const outFile of outFiles) {
  await import(pathToFileURL(outFile).href)
}
