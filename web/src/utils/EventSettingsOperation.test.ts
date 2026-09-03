import assert from 'node:assert/strict'
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import test from 'node:test'

const expectedPaths = [
  'src/pages/admin/games/[id]/Info.tsx',
  'src/pages/admin/games/Index.tsx',
  'src/components/admin/BloodBonusModel.tsx',
]
const expectedSources = expectedPaths.map((path) => ({ path, source: readFileSync(path, 'utf8') }))

const sourceFiles = (directory: string, files: string[] = []): string[] => {
  for (const entry of readdirSync(directory)) {
    const path = join(directory, entry)
    if (statSync(path).isDirectory()) sourceFiles(path, files)
    else if (/\.tsx?$/.test(entry)) files.push(path)
  }
  return files
}

test('every event settings update owns a retry-stable operation ID', () => {
  const allCallers = sourceFiles('src')
    .flatMap((path) =>
      [...readFileSync(path, 'utf8').matchAll(/api\.edit\.editUpdateGame\(/g)].map(() => relative('.', path))
    )
    .sort()

  assert.deepEqual(allCallers, [...expectedPaths].sort())
  for (const { path, source } of expectedSources) {
    assert.match(source, /prepareGameInfoSave\(/, `${path} does not prepare a stable save operation`)
    assert.match(
      source,
      /api\.edit\.editUpdateGame\([^\n]*prepared\.payload/,
      `${path} bypasses the prepared event settings payload`
    )
  }
})
