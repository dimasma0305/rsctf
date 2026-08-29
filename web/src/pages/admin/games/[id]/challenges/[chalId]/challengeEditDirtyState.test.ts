import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

test('challenge editor derives dirty state without a post-hydration state effect', () => {
  const source = readFileSync('src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx', 'utf8')

  assert.match(
    source,
    /const dirty = savedSnapshotRef\.current !== '' && currentSnapshot !== savedSnapshotRef\.current/
  )
  assert.doesNotMatch(source, /setDirty\(/)
  assert.match(source, /if \(!dirty\) return[\s\S]*addEventListener\('beforeunload'/)
})
