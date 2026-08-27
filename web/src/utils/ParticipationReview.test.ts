import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const page = readFileSync('src/pages/admin/games/[id]/Review.tsx', 'utf8')
const api = readFileSync('src/Api.ts', 'utf8')
const styles = readFileSync('src/styles/pages/Review.module.css', 'utf8')

test('participation review sends every list filter and page boundary to the server', () => {
  assert.match(page, /useDebouncedValue\(search\.trim\(\), 300\)/)
  assert.match(page, /useGameParticipations\(numId, participationQuery, OnceSWRConfig, numId > 0\)/)
  assert.match(page, /count: PART_NUM_PER_PAGE/)
  assert.match(page, /skip: \(activePage - 1\) \* PART_NUM_PER_PAGE/)
  assert.match(page, /status: selectedStatus \?\? undefined/)
  assert.match(page, /divisionId: selectedDivisionId/)
  assert.match(page, /search: debouncedSearch \|\| undefined/)
  assert.doesNotMatch(page, /filteredParticipations/)
  assert.doesNotMatch(page, /pagedParticipations/)

  assert.match(api, /gameParticipations:[\s\S]*query: query/)
  assert.match(api, /useGameParticipations:[\s\S]*\[\x60?\/api\/game\/\$\{id\}\/participations\x60?, query\]/)
  assert.match(api, /count\?: number;[\s\S]*skip\?: number;[\s\S]*status\?: ParticipationStatus/)
})

test('roster PII is lazy-loaded only for the opened participation', () => {
  assert.match(page, /useGameParticipationDetail\(gameId, participation\.id, OnceSWRConfig, expanded\)/)
  assert.match(page, /value=\{openedParticipation\}/)
  assert.match(page, /onChange=\{setOpenedParticipation\}/)
  assert.match(page, /expanded=\{openedParticipation === participation\.id\.toString\(\)\}/)
  assert.match(api, /doFetch \? \x60\/api\/game\/\$\{id\}\/participations\/\$\{participationId\}\x60 : null/)

  const summaryStart = api.indexOf('export interface ParticipationReviewSummaryModel')
  const summaryEnd = api.indexOf('export interface ParticipationReviewMemberModel')
  const summary = api.slice(summaryStart, summaryEnd)
  for (const pii of ['email', 'phone', 'realName', 'stdNumber', 'members', 'captainId', 'bio', 'role']) {
    assert.doesNotMatch(summary, new RegExp(`\\b${pii}\\b`, 'i'), `summary exposed ${pii}`)
  }
  const detail = api.slice(summaryEnd, api.indexOf('/** Challenge detailed information */', summaryEnd))
  for (const required of ['email', 'phone', 'realName', 'stdNumber', 'isRegistered', 'isCaptain']) {
    assert.match(detail, new RegExp(`\\b${required}\\b`))
  }
})

test('participation review remains keyboard-readable and stacks filters at compact widths', () => {
  assert.match(page, /component="search"/)
  assert.match(page, /aria-controls="participation-review-results"/)
  assert.match(page, /role="status" aria-live="polite"/)
  assert.match(page, /viewportProps=\{\{[\s\S]*tabIndex: 0,[\s\S]*'aria-label'/)
  assert.match(page, /role="region"[\s\S]*aria-label=/)
  assert.match(page, /detailError[\s\S]*Retry/)
  assert.match(page, /event\.stopPropagation\(\)/)
  assert.match(page, /<ParticipationDivisionEditModal/)
  assert.match(styles, /@media \(max-width: \$mantine-breakpoint-sm\)[\s\S]*\.searchInput,[\s\S]*width: 100%/)
  assert.match(styles, /\.memberValue \{[\s\S]*overflow-wrap: anywhere/)
})
