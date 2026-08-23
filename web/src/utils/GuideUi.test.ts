import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
import test from 'node:test'

const provider = readFileSync('src/components/guide/PlayerGuide.tsx', 'utf8')
const page = readFileSync('src/pages/guide/Index.tsx', 'utf8')
const app = readFileSync('src/App.tsx', 'utf8')
const navigation = readFileSync('src/components/navigation.ts', 'utf8')
const challengeModal = readFileSync('src/components/GameChallengeModal.tsx', 'utf8')
const eventPage = readFileSync('src/pages/games/[id]/Index.tsx', 'utf8')
const config = readFileSync('src/hooks/useConfig.ts', 'utf8')
const pageStyles = readFileSync('src/styles/pages/PlayerGuidePage.module.css', 'utf8')

test('interactive guide is account-scoped, restartable, dismissible, and storage-failure safe', () => {
  assert.match(provider, /guideStorageKey\(identity\)/)
  assert.match(provider, /user\?\.userId/)
  assert.match(provider, /interactiveEnabled/)
  assert.match(provider, /resetGuideProgress/)
  assert.match(provider, /Turn off interactive guide/)
  assert.match(provider, /try \{[\s\S]*localStorage\.setItem[\s\S]*\} catch/)
  assert.match(provider, /<Modal\.Root[\s\S]*returnFocus trapFocus/)
  assert.match(provider, /<Modal\.Title>/)
  assert.doesNotMatch(provider, /<Modal\.Header/)
  assert.match(app, /<PlayerGuideProvider>/)
})

test('guide content follows the effective platform and event connection settings', () => {
  for (const field of [
    'allowRegister',
    'allowPasswordRegistration',
    'emailConfirmationRequired',
    'enableGoogleAuth',
    'enableDiscordAuth',
    'portMapping',
  ]) {
    assert.match(provider, new RegExp(`config\\.${field}`), field)
  }
  assert.match(config, /allowRegister: true/)
  assert.match(config, /emailConfirmationRequired: false/)
  assert.match(challengeModal, /useFeatureGuide\('dynamic-container'/)
  assert.match(challengeModal, /eventVpnRequired/)
  assert.match(eventPage, /useFeatureGuide\([\s\S]*'event-vpn'/)
})

test('permanent guide uses real, described screenshots and remains directly navigable', () => {
  assert.match(navigation, /common\.tab\.guide[\s\S]*\/guide/)
  assert.match(page, /<PageHeader/)
  assert.match(page, /<section id=\{id\}/)
  assert.match(page, /<img src=\{image\} alt=\{imageAlt \?\? ''\}/)
  assert.match(pageStyles, /prefers-reduced-motion/)
  for (const name of ['login.webp', 'games.webp', 'challenge.webp']) {
    assert.ok(existsSync(`public/static/guide/${name}`), name)
  }
})
