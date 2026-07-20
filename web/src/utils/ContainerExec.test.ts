import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { containerExecHubPath } from './ContainerExec'

const containerExecModals = (path: string) => {
  const source = readFileSync(path, 'utf8')
  return source.match(/<ContainerExecModal[\s\S]*?\/>/g) ?? []
}

test('container exec keeps the existing platform Admin path by default', () => {
  assert.equal(containerExecHubPath(), '/hub/containerExec')
})

test('container exec builds one exact game-scoped path', () => {
  assert.equal(containerExecHubPath(37), '/hub/containerExec/games/37')
  for (const invalid of [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.throws(() => containerExecHubPath(invalid), /positive game id/)
  }
})

test('game-owned terminal callsites use scoped hubs while Instances stays platform-wide', () => {
  for (const path of [
    'src/pages/admin/games/[id]/AdOps.tsx',
    'src/pages/admin/games/[id]/challenges/[chalId]/Index.tsx',
  ]) {
    const modals = containerExecModals(path)
    assert.equal(modals.length, 1, `${path} has one terminal modal`)
    assert.match(modals[0], /scopedGameId=\{numId\}/, `${path} uses its game-scoped hub`)
  }

  const platformModals = containerExecModals('src/pages/admin/Instances.tsx')
  assert.equal(platformModals.length, 1, 'Instances has one terminal modal')
  assert.doesNotMatch(platformModals[0], /scopedGameId=/, 'Instances keeps the platform Admin hub')
})
