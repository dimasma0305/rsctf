import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
import test from 'node:test'

const provider = readFileSync('src/components/guide/PlayerGuide.tsx', 'utf8')
const spotlight = readFileSync('src/components/guide/GuideSpotlightModal.tsx', 'utf8')
const spotlightStyles = readFileSync('src/styles/components/PlayerGuide.module.css', 'utf8')
const guideLayout = readFileSync('src/utils/GuideLayout.ts', 'utf8')
const page = readFileSync('src/pages/guide/Index.tsx', 'utf8')
const app = readFileSync('src/App.tsx', 'utf8')
const navigation = readFileSync('src/components/navigation.ts', 'utf8')
const challengeModal = readFileSync('src/components/GameChallengeModal.tsx', 'utf8')
const instanceEntry = readFileSync('src/components/InstanceEntry.tsx', 'utf8')
const eventPage = readFileSync('src/pages/games/[id]/Index.tsx', 'utf8')
const appHeader = readFileSync('src/components/AppHeader.tsx', 'utf8')
const appNavbar = readFileSync('src/components/AppNavbar.tsx', 'utf8')
const teamsPage = readFileSync('src/pages/Teams.tsx', 'utf8')
const challengeCard = readFileSync('src/components/ChallengeCard.tsx', 'utf8')
const config = readFileSync('src/hooks/useConfig.ts', 'utf8')
const pageStyles = readFileSync('src/styles/pages/PlayerGuidePage.module.css', 'utf8')

test('interactive guide is account-scoped, restartable, dismissible, and storage-failure safe', () => {
  assert.match(provider, /guideStorageKey\(identity\)/)
  assert.match(provider, /user\?\.userId/)
  assert.match(provider, /interactiveEnabled/)
  assert.match(provider, /resetGuideProgress/)
  assert.match(provider, /preferences\.activeTourStep/)
  assert.match(provider, /updatePreferences\(pauseGuide\)/)
  assert.match(provider, /setGuideTourStep/)
  assert.doesNotMatch(provider, /setTourOpen/)
  assert.match(provider, /Stop guide/)
  assert.match(provider, /try \{[\s\S]*localStorage\.setItem[\s\S]*\} catch/)
  assert.match(spotlight, /<Modal\.Root[\s\S]*returnFocus[\s\S]*trapFocus/)
  assert.match(spotlight, /trapFocus=\{!target && !yielding\}/)
  assert.match(spotlight, /setAttribute\('aria-modal', target \|\| yielding \? 'false' : 'true'\)/)
  assert.match(spotlight, /data-autofocus/)
  assert.match(spotlight, /bodyRef\.current\?\.focus\(\{ preventScroll: true \}\)/)
  assert.match(spotlight, /onEnterTransitionEnd=/)
  assert.match(spotlight, /<Modal\.Title>/)
  assert.doesNotMatch(spotlight, /<Modal\.Header/)
  assert.match(app, /<PlayerGuideProvider>/)
})

test('interactive guide spotlights real controls and provides a reduced-motion game cursor', () => {
  assert.match(provider, /targetSelector:/)
  assert.match(instanceEntry, /data-guide="instance-start"/)
  assert.match(instanceEntry, /data-guide="instance-entry"/)
  assert.match(appHeader, /data-guide="more-navigation"/)
  assert.match(provider, /guide-navigation[\s\S]*more-navigation/)
  assert.match(spotlight, /selector[\s\S]*\.split\(','\)[\s\S]*querySelectorAll<HTMLElement>\(candidate\)/)
  assert.match(spotlight, /scrollIntoView\(/)
  assert.match(spotlight, /targetVisibleRatio\(element\) >= 0\.6/)
  assert.match(spotlight, /document\.elementsFromPoint/)
  assert.match(spotlight, /renderedTargets\(selector\)\?\.\[0\]/)
  assert.match(spotlight, /prefers-reduced-motion: reduce/)
  assert.match(spotlight, /mdiCursorDefaultClickOutline/)
  assert.match(spotlight, /data-guide-layer="spotlight"/)
  assert.match(spotlight, /data-guide-layer="cursor"/)
  assert.match(spotlight, /data-guide-layer="interaction-blocker"/)
  assert.match(spotlight, /data-guide-surface="coachmark"/)
  assert.match(spotlight, /data-guide-placement=/)
  assert.match(spotlight, /document\.addEventListener\('click', handleTargetClick, true\)/)
  assert.match(spotlight, /onTargetActivate\(guideTarget\)/)
  assert.match(spotlight, /guideLayerZIndex\(target\)/)
  assert.match(spotlight, /\[role="dialog"\]/)
  assert.match(spotlight, /externalSurface && !target\?\.elevated/)
  assert.match(spotlight, /trapFocus=\{!target && !yielding\}/)
  assert.match(spotlight, /data-guide-yielding=\{yielding \|\| undefined\}/)
  assert.match(spotlight, /aria-hidden=\{yielding \|\| undefined\}/)
  assert.match(spotlight, /closeOnClickOutside=\{false\}/)
  assert.match(spotlight, /pointerEvents: target \|\| yielding \? 'none'/)
  assert.match(spotlight, /<Modal\.Body[\s\S]*tabIndex=\{0\}[\s\S]*aria-label=\{title\}/)
  assert.match(spotlight, /fillRule="evenodd"/)
  assert.match(spotlightStyles, /\.tutorialSpotlight/)
  assert.match(spotlightStyles, /\.tutorialCursor/)
  assert.match(spotlightStyles, /\.tutorialBlocker/)
  assert.match(spotlightStyles, /prefers-reduced-motion[\s\S]*\.tutorialSpotlight/)
  assert.match(spotlightStyles, /max-height: min\(19rem, 44dvh\)/)
  assert.match(provider, /size="min\(21rem, calc\(100vw - 1rem\)\)"/)
  assert.doesNotMatch(provider, /size="min\(36rem/)
  assert.match(guideLayout, /target\.viewportHeight \* 0\.44/)
  assert.match(guideLayout, /GUIDE_PAGE_Z_INDEX = 150/)
  assert.match(guideLayout, /GUIDE_ELEVATED_Z_INDEX = 500/)
  assert.match(guideLayout, /MOBILE_TOP_SAFE_INSET = 76/)
  assert.match(guideLayout, /MOBILE_BOTTOM_SAFE_INSET = 82/)
  assert.doesNotMatch(spotlightStyles, /z-index: 700/)
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
  assert.match(page, /<section[\s\S]*id=\{id\}/)
  assert.match(page, /<img src=\{image\} alt=\{imageAlt \?\? ''\}/)
  assert.match(page, /<ol className=\{classes\.instructionGrid\}/)
  assert.match(page, /<figcaption className=\{classes\.instructionCaption\}/)
  assert.match(page, /<img src=\{step\.image\} alt=\{step\.imageAlt\}/)
  assert.match(page, /mdiCursorDefaultClickOutline/)
  assert.match(page, /className=\{classes\.instructionCursor\}/)
  assert.doesNotMatch(page, /<Badge[^>]*circle/)
  assert.match(pageStyles, /prefers-reduced-motion/)
  for (const name of [
    'login.webp',
    'games.webp',
    'challenge.webp',
    'join-event.webp',
    'join-confirm.webp',
    'join-team.webp',
    'join-status.webp',
  ]) {
    assert.ok(existsSync(`public/static/guide/${name}`), name)
  }
})

test('the event tutorial resumes with destination-specific instructions after a game-card navigation', () => {
  assert.match(provider, /isGameDetailPage[\s\S]*event-join[\s\S]*event-challenges/)
  assert.match(provider, /event-card[\s\S]*games-search/)
  assert.match(provider, /Review the schedule and rules/)
  assert.match(eventPage, /data-guide="event-briefing"/)
  assert.match(eventPage, /data-guide="event-join"/)
  assert.match(eventPage, /data-guide="event-challenges"/)
})

test('the novice path teaches every action on the real player controls', () => {
  assert.match(provider, /id: 'team'/)
  assert.match(provider, /Create or join a team/)
  assert.match(provider, /team-create[\s\S]*team-join/)
  assert.match(provider, /targetSelector:[\s\S]*challenge-card/)
  assert.match(provider, /instance-start[\s\S]*instance-entry/)
  assert.match(provider, /flag-submit/)
  assert.match(appNavbar, /team-navigation/)
  assert.match(appHeader, /team-navigation/)
  assert.match(teamsPage, /data-guide="team-create"/)
  assert.match(teamsPage, /data-guide="team-join"/)
  assert.match(challengeCard, /data-guide="challenge-card"/)
})
