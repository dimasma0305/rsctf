import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { describe, it } from 'node:test'
import { progressLabel } from '../utils/SnapshotDownload'

describe('snapshot download progress', () => {
  it('reports bounded percentages and an indeterminate fallback', () => {
    assert.equal(progressLabel(1, 0), '…')
    assert.equal(progressLabel(25, 100), '25%')
    assert.equal(progressLabel(200, 100), '100%')
  })
})

it('player and admin snapshots use the shared guarded control instead of plain anchors', () => {
  for (const path of ['src/components/AdChallengePanel.tsx', 'src/pages/admin/games/[id]/AdOps.tsx']) {
    const source = readFileSync(path, 'utf8')
    assert.match(source, /<SnapshotDownloadButton/)
  }
  const control = readFileSync('src/components/SnapshotDownloadButton.tsx', 'utf8')
  assert.ok(control.indexOf('starting.current = true') < control.indexOf('void runDownloadSingleFlight('))

  const writeups = readFileSync('src/pages/admin/games/[id]/Writeups.tsx', 'utf8')
  assert.match(writeups, /downloadBlob\(/)
  assert.doesNotMatch(writeups, /window\.open\([^)]*writeups/)
})
