import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

test('login entry points preserve query and hash deep links', () => {
  for (const path of [
    'components/AppHeader.tsx',
    'components/AppNavbar.tsx',
    'components/WithRole.tsx',
    'pages/account/Profile.tsx',
    'pages/challenges/Index.tsx',
  ]) {
    assert.match(
      readFileSync(`src/${path}`, 'utf8'),
      /encodeURIComponent\(location.pathname \+ location.search \+ location.hash\)/,
      path
    )
  }
})
