import assert from 'node:assert/strict'
import test from 'node:test'
import { adTokenViewerScope, isCurrentAdTokenViewer } from './AdTokenScope'

test('participation-bound A&D tokens reject a stale participation response', () => {
  const requested = adTokenViewerScope({ participationId: 4, teamId: 8 })
  assert.equal(
    isCurrentAdTokenViewer(
      requested,
      { participationId: 4, teamId: 8 },
      adTokenViewerScope({ participationId: 5, teamId: 9 })
    ),
    false
  )
})

test('participation-bound A&D tokens accept only an exact viewer scope', () => {
  const scope = { participationId: 4, teamId: 8 }
  assert.equal(
    isCurrentAdTokenViewer(adTokenViewerScope(scope), scope, adTokenViewerScope(scope)),
    true
  )
})
