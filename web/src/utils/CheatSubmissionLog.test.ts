import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

const source = readFileSync('src/components/monitor/CheatSubmissionLog.tsx', 'utf8')
const feedHook = readFileSync('src/hooks/useCheatIncidentFeed.ts', 'utf8')

test('the suspicious submission feed is capped, reconnect-safe, and exposes request failures', () => {
  assert.match(source, /useCheatIncidentFeed\(gameId, active\)/)
  assert.match(feedHook, /INCIDENT_PAGE_SIZE = 100/)
  assert.match(feedHook, /afterId/)
  assert.match(feedHook, /beforeObservedAt/)
  assert.match(feedHook, /beforeId/)
  assert.match(feedHook, /refreshInterval: cadence/)
  assert.match(source, /if \(error && cheatInfo\.length === 0\)/)
  assert.match(source, /tryGetErrorMsg\(error, t\)/)
  assert.match(source, /onClick=\{\(\) => void refresh\(\)\}/)
  assert.match(source, /Load older incidents/)
})

test('the suspicious submission feed does not mutate SWR data and uses deterministic row keys', () => {
  assert.doesNotMatch(source, /props\.cheatInfo\s*\.sort\(/)
  assert.match(source, /const occurrences = new Map<string, number>\(\)/)
  assert.match(source, /key: JSON\.stringify\(\[signature, occurrence\]\)/)
  assert.match(source, /key=\{submissionInfo\.key\}/)
  assert.match(source, /useMemo\(\(\) => ToCheatTeamInfo\(cheatInfo\), \[cheatInfo\]\)/)
})

test('both suspicious submission views are named keyboard-accessible regions with an empty state', () => {
  const namedRegions = source.match(
    /viewportProps=\{\{[\s\S]*?role:\s*'region',[\s\S]*?tabIndex:\s*0,[\s\S]*?'aria-label':[\s\S]*?\}\}/g
  )

  assert.equal(namedRegions?.length, 2)
  assert.match(source, /const CheatSubmissionEmptyState/)
  assert.match(source, /role="status" aria-live="polite"/)
  assert.match(source, /<Table\.Td colSpan=\{7\}>/)
})
