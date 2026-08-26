// Minimal test runner for the ClientApp: finds every `*.test.ts` under src/,
// transpiles each with esbuild, and runs them through Node's built-in test runner
// (node:test). Component tests opt into happy-dom explicitly; pure-logic tests do
// not pay for a shared browser environment. Exits non-zero if any test fails.
import { build } from 'esbuild'
import { mkdirSync, mkdtempSync, readdirSync, rmSync, statSync } from 'node:fs'
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

const testOutputRoot = join('node_modules', '.tmp')
mkdirSync(testOutputRoot, { recursive: true })
const outDir = mkdtempSync(join(testOutputRoot, 'rsctf-web-test-'))
const outFiles = []
try {
  let i = 0
  for (const entry of entries) {
    const outFile = join(outDir, `test-${i++}.mjs`)
    await build({
      entryPoints: [entry],
      outfile: outFile,
      bundle: true,
      platform: 'node',
      format: 'esm',
      tsconfig: 'tsconfig.app.json',
      // Keep the browser renderer's scheduler out of the test bundle. Bundling
      // it selects its browser MessageChannel path, which leaves Node ports open.
      external: [
        '@mantine/core',
        'happy-dom',
        'i18next',
        'react',
        'react/*',
        'react-dom',
        'react-dom/*',
        'react-i18next',
      ],
    })
    outFiles.push(outFile)
  }

  // Importing a bundled module registers its node:test cases; the runner executes
  // them at process exit and sets a non-zero exit code on any failure.
  for (const outFile of outFiles) {
    await import(pathToFileURL(outFile).href)
  }
} finally {
  rmSync(outDir, { recursive: true, force: true })
}
