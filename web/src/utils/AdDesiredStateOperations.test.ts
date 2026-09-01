import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const page = readFileSync('src/pages/admin/games/[id]/AdOps.tsx', 'utf8')
const hills = readFileSync('src/components/admin/KothOpsPanel.tsx', 'utf8')

test('live scoring controls own one explicit pending intent per resource', () => {
  assert.match(page, /hillCommandRef = useRef\(new Map<number, Promise<void>>\(\)\)/)
  assert.match(page, /const active = hillCommandRef\.current\.get\(hill\.challengeId\)[\s\S]*if \(active\) return active/)
  assert.match(page, /const desiredEnabled = !hill\.isEnabled/)
  assert.match(page, /new Map\(current\)\.set\(hill\.challengeId, desiredEnabled\)/)
  assert.match(hills, /const displayedEnabled = pendingEnabled \?\? hill\.isEnabled/)
  assert.match(hills, /checked=\{displayedEnabled\}[\s\S]*disabled=\{pendingEnabled !== undefined\}/)

  assert.match(page, /scoringCommandRef = useRef<Promise<void> \| null>\(null\)/)
  assert.match(page, /if \(scoringCommandRef\.current\) return scoringCommandRef\.current/)
  assert.match(page, /const desiredPaused = !snapshot\.scoringPaused/)
  assert.match(page, /setPendingScoringPaused\(desiredPaused\)/)
  assert.match(page, /pendingScoringPaused === true[\s\S]*Pausing scoring…/)
  assert.match(page, /pendingScoringPaused === false[\s\S]*Resuming scoring…/)
})
