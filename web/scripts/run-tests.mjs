// Minimal test runner for the ClientApp: finds every `*.test.ts` under src/,
// transpiles each with esbuild, and runs them through Node's built-in test runner
// (node:test). Component tests opt into happy-dom explicitly; pure-logic tests do
// not pay for a shared browser environment. Exits non-zero if any test fails.
import { build } from 'esbuild'
import { spawn } from 'node:child_process'
import { mkdirSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { once } from 'node:events'
import { join, relative } from 'node:path'
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
      // The collector is a browser-only dynamic chunk. Join-dialog tests cover
      // its recoverable failure boundary without initializing CreepJS in Node.
      '@Utils/BrowserFingerprint',
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

  // Keep the generated modules alive until node:test has finished every async
  // test. Importing them in this parent process lets the finally block race the
  // runner and remove fixtures while tests are still reading them.
  const runner = join(outDir, 'runner.mjs')
  writeFileSync(
    runner,
    outFiles.map((outFile) => `await import(${JSON.stringify(pathToFileURL(outFile).href)})`).join('\n'),
  )
  const child = spawn(process.execPath, [runner], { stdio: 'inherit' })
  const [code, signal] = await once(child, 'exit')
  if (code !== 0) {
    throw new Error(`web test runner exited with ${code ?? `signal ${signal}`}`)
  }
} finally {
  rmSync(outDir, { recursive: true, force: true })
}
