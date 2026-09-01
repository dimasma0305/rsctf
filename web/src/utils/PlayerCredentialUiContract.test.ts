import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const challengePage = readFileSync('src/pages/games/[id]/Challenges.tsx', 'utf8')
const tokenOwner = readFileSync('src/components/AdToolkitSections.tsx', 'utf8')
const adGuide = readFileSync('src/components/AdGuideModal.tsx', 'utf8')
const kothGuide = readFileSync('src/components/KothGuideModal.tsx', 'utf8')
const kothHill = readFileSync('src/components/KothChallengePanel.tsx', 'utf8')

test('hybrid A&D and KotH toolkits share one page-scoped token mutation owner', () => {
  assert.equal(challengePage.match(/useAdToken\(/g)?.length, 1)
  assert.equal(challengePage.match(/tokenOwner=\{tokenOwner\}/g)?.length, 2)
  assert.equal(adGuide.includes('useAdToken('), false)
  assert.equal(kothGuide.includes('useAdToken('), false)
})

test('every one-time credential surface sends a revision fence and checks response ownership', () => {
  for (const source of [tokenOwner, adGuide, kothHill]) {
    assert.match(source, /operationId/)
    assert.match(source, /expectedRevision/)
    assert.match(source, /ownsPlayerCredentialResult/)
    assert.match(source, /responseGeneration|sshResponseGeneration/)
  }
})

test('SSH generation rejects a late one-time response after an account switch', () => {
  assert.match(adGuide, /playerCredentialOperationStorageKey\(viewerScopeAtStart, gameId, 'ad-ssh'\)/)
  assert.match(adGuide, /activeSshViewerScope\.current !== viewerScopeAtStart/)
  assert.match(adGuide, /SSH credential response for an older account was ignored/)
})

test('credential plaintext remains in React session memory rather than web storage', () => {
  for (const source of [tokenOwner, adGuide, kothGuide, kothHill]) {
    assert.equal(source.includes('localStorage'), false)
    assert.equal(source.includes('sessionStorage'), false)
  }
  assert.match(tokenOwner, /setStoredToken\(result\.token\)/)
  assert.match(adGuide, /setFreshPrivKey\(/)
})
