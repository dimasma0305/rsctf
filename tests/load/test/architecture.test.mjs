import assert from 'node:assert/strict'
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { checkArchitecture } from '../../../scripts/check-architecture.mjs'

test('architecture check rejects new service-to-controller calls and oversized modules', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'rsctf-architecture-'))
  try {
    await mkdir(path.join(root, 'scripts'))
    await mkdir(path.join(root, 'src/services'), { recursive: true })
    await writeFile(path.join(root, 'scripts/architecture-legacy.json'), '{}\n')
    await writeFile(path.join(root, 'src/services/example.rs'), 'fn example() { crate::controllers::edit::mutate(); }\n')
    assert.equal((await checkArchitecture(root)).errors.length, 1)
    await writeFile(path.join(root, 'src/services/example.rs'), '// A comment about controllers::edit::mutate\nfn example() {}\n')
    assert.deepEqual((await checkArchitecture(root)).errors, [])
    await writeFile(path.join(root, 'src/services/example.rs'), '// line\n'.repeat(1001))
    assert.match((await checkArchitecture(root)).errors[0], /1000-line/)
  } finally { await rm(root, { recursive: true }) }
})
