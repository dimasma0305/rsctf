import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

test('compact challenge categories keep the active keyboard tab inside the scroller', () => {
  const source = readFileSync('src/components/ChallengePanel.tsx', 'utf8')

  assert.match(source, /const categoryTabsRef = useRef<HTMLDivElement>\(null\)/)
  assert.match(source, /querySelector<HTMLElement>\('\[role="tab"\]\[data-active\]'/)
  assert.match(source, /Math\.min\(maximum, Math\.max\(0, centered\)\)/)
  assert.match(source, /<Tabs\.List\s+ref=\{categoryTabsRef\}\s+data-challenge-category-tabs/)
})
