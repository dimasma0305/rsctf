import { describe, expect, it } from 'vitest'
import { adTokenViewerScope, isCurrentAdTokenViewer } from './AdTokenScope'

describe('participation-bound A&D plaintext tokens', () => {
  it('rejects a response after the active participation changes', () => {
    const requested = adTokenViewerScope({ participationId: 4, teamId: 8 })
    expect(
      isCurrentAdTokenViewer(
        requested,
        { participationId: 4, teamId: 8 },
        adTokenViewerScope({ participationId: 5, teamId: 9 })
      )
    ).toBe(false)
  })

  it('accepts only an exact request, result, and current scope match', () => {
    const scope = { participationId: 4, teamId: 8 }
    expect(isCurrentAdTokenViewer(adTokenViewerScope(scope), scope, adTokenViewerScope(scope))).toBe(true)
  })
})
