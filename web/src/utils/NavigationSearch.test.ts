import assert from 'node:assert/strict'
import test from 'node:test'
import { searchNavigation } from './NavigationSearch'

const pages = [
  { title: 'Team', section: 'Player workspace', link: '/teams', keywords: '' },
  { title: 'Teams', section: 'Administration', link: '/admin/teams', keywords: '' },
  {
    title: 'Account policy',
    section: 'Settings',
    link: '/admin/settings?section=account',
    keywords: 'registration login',
  },
  { title: 'Résumé', section: 'Player workspace', link: '/summary', keywords: '' },
]

test('navigation search matches multiple words in any order across title and context', () => {
  assert.deepEqual(searchNavigation(pages, 'admin teams'), [pages[1]])
  assert.deepEqual(searchNavigation(pages, '  policy   login  '), [pages[2]])
  assert.deepEqual(searchNavigation(pages, 'login policy'), [pages[2]])
  assert.deepEqual(searchNavigation(pages, 'resume'), [pages[3]])
  assert.deepEqual(searchNavigation(pages, 'unknown destination'), [])
})

test('navigation search prioritizes exact titles and keeps workspace order on ties', () => {
  const contexts = [pages[1], pages[0]]
  assert.deepEqual(searchNavigation(contexts, ''), contexts)
  assert.deepEqual(searchNavigation(contexts, '  '), contexts)
  assert.deepEqual(searchNavigation(contexts, 'team'), [pages[0], pages[1]])
  assert.deepEqual(searchNavigation(contexts, '/teams'), contexts)
  assert.deepEqual(contexts, [pages[1], pages[0]], 'search must not reorder its input')
})

test('navigation search cannot introduce a destination absent from the permitted list', () => {
  assert.deepEqual(searchNavigation([pages[0]], 'admin'), [])
  assert.deepEqual(searchNavigation([], ''), [])
})
