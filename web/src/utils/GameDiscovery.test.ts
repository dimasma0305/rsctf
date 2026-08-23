import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const page = readFileSync('src/pages/games/Index.tsx', 'utf8')
const card = readFileSync('src/components/GameCard.tsx', 'utf8')
const cardStyles = readFileSync('src/styles/components/GameCard.module.css', 'utf8')
const api = readFileSync('src/Api.ts', 'utf8')

test('event discovery searches the complete server-side catalog accessibly', () => {
  assert.match(page, /role="search"/)
  assert.match(page, /label=\{t\('game\.content\.search_label'/)
  assert.match(page, /useDebouncedValue\(search\.trim\(\), 300\)/)
  assert.match(page, /search: debouncedSearch \|\| undefined/)
  assert.match(page, /aria-controls="event-catalog-results"/)
  assert.match(page, /role="status" aria-live="polite"/)
  assert.match(page, /setPage\(1\)/)
  assert.match(api, /Case-insensitive event title, summary, or exact ID search/)
})

test('event cards remain whole-card links without a redundant view-event footer', () => {
  assert.match(card, /<Link to=\{`\/games\/\$\{game\.id\}`\} className=\{classes\.link\}>/)
  assert.doesNotMatch(card, /view_event|mdiArrowRight|classes\.action/)
  assert.doesNotMatch(cardStyles, /\.action/)
})
