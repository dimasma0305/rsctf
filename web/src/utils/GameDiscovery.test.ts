import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const page = readFileSync('src/pages/games/Index.tsx', 'utf8')
const card = readFileSync('src/components/GameCard.tsx', 'utf8')
const cardStyles = readFileSync('src/styles/components/GameCard.module.css', 'utf8')
const api = readFileSync('src/Api.ts', 'utf8')

test('event discovery provides retry and filter reset instead of indefinite loading on errors', () => {
  assert.match(page, /error: gamesError/)
  assert.match(page, /gamesError &&/)
  assert.match(page, /role="alert"/)
  assert.match(page, /!gamesError &&/)
  assert.match(page, /mutate\(\)\.catch/)
  assert.match(page, /game\.content\.reset_filters/)
  assert.match(page, /setMembership\(GameMembershipFilter\.All\)/)
})

test('event discovery searches the complete server-side catalog accessibly', () => {
  assert.match(page, /role="search"/)
  assert.match(page, /label=\{t\('game\.content\.search_label'/)
  assert.match(page, /useDebouncedValue\(search\.trim\(\), 300\)/)
  assert.match(page, /search: debouncedSearch \|\| undefined/)
  assert.match(page, /aria-controls="event-catalog-results"/)
  assert.match(page, /role="status" aria-live="polite"/)
  assert.match(page, /setPage\(1\)/)
  assert.match(api, /Case-insensitive event title, summary, or exact ID search/)
  assert.match(page, /GameMembershipFilter\.Joined/)
  assert.match(page, /GameMembershipFilter\.NotJoined/)
  assert.match(card, /participationStatus/)
  assert.match(card, /showMembership/)
})

test('global challenge discovery remains authenticated and event-membership scoped in its copy and API', () => {
  const challengePage = readFileSync('src/pages/challenges/Index.tsx', 'utf8')
  const navigation = readFileSync('src/components/navigation.ts', 'utf8')
  const mobileHeader = readFileSync('src/components/AppHeader.tsx', 'utf8')
  const mobileHeaderStyles = readFileSync('src/styles/components/AppHeader.module.css', 'utf8')
  assert.match(challengePage, /Navigate to="\/account\/login\?from=%2Fchallenges"/)
  assert.match(challengePage, /useGameChallengeCatalog/)
  assert.match(challengePage, /value: 'jeopardy', label: 'Jeopardy'/)
  assert.match(challengePage, /value: 'koth', label: 'KOTH'/)
  assert.match(challengePage, /value: 'attackDefense', label: 'A&D'/)
  assert.doesNotMatch(challengePage, /Object\.values\(ChallengeType\)/)
  assert.match(challengePage, /category: category \?\? undefined/)
  assert.match(challengePage, /mode: challengeMode \?\? undefined/)
  assert.match(challengePage, /solved: solveFilter/)
  assert.match(challengePage, /Upcoming, hidden, and unauthorized event content stays private/)
  assert.match(challengePage, /<ChallengeCard/)
  assert.match(challengePage, /<CatalogChallengeModal/)
  assert.match(challengePage, /eventHref=\{eventHref\}/)
  assert.doesNotMatch(challengePage, /withCloseButton/)
  const challengeModal = readFileSync('src/components/ChallengeModal.tsx', 'utf8')
  assert.match(challengeModal, /<Title order=\{2\} size="h4"/)
  assert.doesNotMatch(challengeModal, /<Modal\.Header>/)
  assert.doesNotMatch(challengePage, /<Link[\s\S]*to=\{`\/games\/\$\{challenge\.gameId\}/)
  assert.match(navigation, /link: '\/challenges',[\s\S]*requiresAuth: true/)
  assert.match(navigation, /dockLabel: 'common\.tab\.challenge_catalog_short'/)
  assert.match(mobileHeader, /t\(item\.dockLabel \?\? item\.label\)/)
  assert.match(mobileHeaderStyles, /\.dockItem > :global\(\.mantine-Text-root\)[\s\S]*text-overflow: ellipsis/)
})

test('event cards remain whole-card links without a redundant view-event footer', () => {
  assert.match(card, /<Link to=\{`\/games\/\$\{game\.id\}`\} className=\{classes\.link\} data-guide="event-card">/)
  assert.doesNotMatch(card, /view_event|mdiArrowRight|classes\.action/)
  assert.doesNotMatch(cardStyles, /\.action/)
})

test('event status and membership use wrapping content instead of clipped poster overlays', () => {
  assert.match(card, /classes\.content[\s\S]*classes\.stateRow[\s\S]*data-status=\{status\}/)
  assert.match(card, /classes\.stateRow[\s\S]*data-membership=\{membership\.status\}/)
  const stateStyles = cardStyles.match(/\.status,\s*\.membership \{([\s\S]*?)\n\}/)?.[1]
  assert.ok(stateStyles)
  assert.match(stateStyles, /overflow-wrap: anywhere/)
  assert.doesNotMatch(stateStyles, /position: absolute|overflow: hidden|white-space: nowrap/)
  assert.match(cardStyles, /\.stateRow \{[\s\S]*?flex-wrap: wrap/)
})
