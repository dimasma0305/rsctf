// Minimal test runner for the ClientApp: finds every `*.test.ts` under src/,
// transpiles each with esbuild, and runs them through Node's built-in test runner
// (node:test). Component tests opt into happy-dom explicitly; pure-logic tests do
// not pay for a shared browser environment. Exits non-zero if any test fails.
import { build } from 'esbuild'
import { spawn } from 'node:child_process'
import { mkdirSync, mkdtempSync, readdirSync, rmSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'

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

// Node's test discovery deliberately skips paths below node_modules, including
// explicitly provided generated test files. Keep bundles in a web-local,
// ignored directory so the child runner executes every entry.
const testOutputRoot = '.test-dist'
mkdirSync(testOutputRoot, { recursive: true })
const outDir = mkdtempSync(join(testOutputRoot, 'rsctf-web-test-'))
try {
  // Compile all entries in one build so esbuild shares discovery and worker
  // startup instead of repeating that work once per test file.
  await build({
    entryPoints: entries,
    outbase: 'src',
    outdir: outDir,
    outExtension: { '.js': '.mjs' },
    bundle: true,
    platform: 'node',
    format: 'esm',
    banner: {
      js: "import { createRequire as __createRequire } from 'node:module'; const require = __createRequire(import.meta.url);",
    },
    tsconfig: 'tsconfig.app.json',
    define: {
      'import.meta.env': JSON.stringify({
        DEV: false,
        VITE_APP_BUILD_TIMESTAMP: 'test',
        VITE_APP_GIT_NAME: 'test',
        VITE_APP_GIT_SHA: 'test',
      }),
    },
    // Browser font assets have no behavior in happy-dom. Treat them as empty
    // modules so component regressions can import the real Markdown/KaTeX
    // surface without copying production components into a test double.
    loader: {
      '.ttf': 'empty',
      '.woff': 'empty',
      '.woff2': 'empty',
    },
    // Keep the browser renderer's scheduler out of the test bundle. Bundling
    // it selects its browser MessageChannel path, which leaves Node ports open.
    external: [
      '@mantine/core',
      'axios',
      'happy-dom',
      'i18next',
      'react',
      'react/*',
      'react-dom',
      'react-dom/*',
      'react-i18next',
      'swr',
    ],
  })
  const outFiles = entries.map((entry) => join(outDir, relative('src', entry).replace(/\.ts$/, '.mjs')))

  // Keep the generated modules alive until node:test has completed every file.
  // Importing them in this process only registers tests for a later event-loop
  // turn, which lets the finally block remove files that a test still resolves
  // relative to import.meta.url. A bounded child runner gives cleanup an exact
  // completion boundary and avoids unbounded per-file worker churn.
  const testArgs = ['--test', '--test-concurrency=2']
  const namePattern = process.env.RSCTF_WEB_TEST_NAME_PATTERN?.trim()
  if (namePattern) testArgs.push(`--test-name-pattern=${namePattern}`)
  testArgs.push(...outFiles)
  const status = await new Promise((resolve, reject) => {
    const child = spawn(process.execPath, testArgs, {
      stdio: 'inherit',
    })
    child.once('error', reject)
    child.once('exit', (code, signal) => {
      if (signal) reject(new Error(`frontend test runner terminated by ${signal}`))
      else resolve(code ?? 1)
    })
  })
  if (status !== 0) process.exitCode = status
} finally {
  rmSync(outDir, { recursive: true, force: true })
}
