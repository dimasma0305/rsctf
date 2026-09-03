import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const api = readFileSync('src/Api.ts', 'utf8')
const config = readFileSync('src/hooks/useConfig.ts', 'utf8')
const settings = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
const teams = readFileSync('src/pages/Teams.tsx', 'utf8')

test('organizers can disable player team creation without hiding team joins', () => {
  assert.equal((api.match(/allowTeamCreation\?: boolean/g) ?? []).length, 2)
  assert.match(config, /allowTeamCreation: true/)
  assert.match(settings, /checked=\{accountPolicy\?\.allowTeamCreation \?\? true\}/)
  assert.match(settings, /allowTeamCreation: e\.currentTarget\.checked/)
  assert.match(teams, /const allowTeamCreation = config\.allowTeamCreation !== false/)
  assert.match(teams, /\{allowTeamCreation && \(\s*<Button[\s\S]*data-guide="team-create"/)
  assert.match(teams, /data-guide="team-join"/)
  assert.match(teams, /\{allowTeamCreation && \(\s*<TeamCreateModal/)
})
