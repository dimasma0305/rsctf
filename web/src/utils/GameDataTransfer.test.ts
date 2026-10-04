import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const api = readFileSync('src/Api.ts', 'utf8')
const infoPage = readFileSync('src/pages/admin/games/[id]/Info.tsx', 'utf8')
const gamesPage = readFileSync('src/pages/admin/games/Index.tsx', 'utf8')

const contractSection = (start: string, end: string) => {
  const startIndex = api.indexOf(start)
  const endIndex = api.indexOf(end, startIndex)
  assert.notEqual(startIndex, -1, `missing API contract start: ${start}`)
  assert.notEqual(endIndex, -1, `missing API contract end: ${end}`)
  return api.slice(startIndex, endIndex)
}

test('competition data export and import use the fixed edit routes and typed result', () => {
  const exportData = contractSection('editExportGameData: (', 'editFlushScoreboardCache: (')
  const importData = contractSection('editImportGameData: (', 'editRemoveFlag: (')

  assert.match(exportData, /path: `\/api\/edit\/games\/\$\{id\}\/export\/data`/)
  assert.match(exportData, /method: "POST"/)
  assert.match(exportData, /attachments\?: "bundle" \| "skip"/)
  assert.match(importData, /this\.request<GameDataImportResult, RequestResponse>/)
  assert.match(importData, /path: `\/api\/edit\/games\/import\/data`/)
  assert.match(importData, /type: ContentType\.FormData/)

  const result = contractSection(
    'export interface GameDataImportResult {',
    'export interface AdminUserImportRowResult {'
  )
  for (const field of ['gameId: number', 'title: string', 'sourceGameId: number', 'exportedAtUtc: number']) {
    assert.match(result, new RegExp(field.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  }
  assert.match(result, /tables: GameDataImportTable\[\]/)
  assert.match(result, /users: GameDataImportRoster/)
  assert.match(result, /teams: GameDataImportRoster/)
  assert.match(
    api,
    /export interface GameDataImportTable \{[\s\S]*name: string;[\s\S]*rows: number;[\s\S]*restored: boolean;/
  )
  assert.match(api, /export interface GameDataImportRoster \{[\s\S]*matched: number;[\s\S]*created: number;/)
})

test('admin pages wire the data export download and the data import upload', () => {
  assert.match(
    infoPage,
    /downloadBlob\(\s*`admin:game-data-export:\$\{gameId\}`,[\s\S]*api\.edit\.editExportGameData\(gameId, undefined, \{ format: 'blob' \}\)[\s\S]*status !== 413[\s\S]*editExportGameData\(gameId, \{ attachments: 'skip' \}, \{ format: 'blob' \}\)/
  )
  assert.match(infoPage, /t\('admin\.button\.games\.export_data'\)/)

  assert.match(gamesPage, /<FileButton onChange=\{onImportGameData\} accept="application\/zip">/)
  assert.match(gamesPage, /api\.edit\.editImportGameData\(/)
  assert.match(gamesPage, /table\.restored \? sum \+ table\.rows : sum/)
  assert.match(gamesPage, /navigate\(`\/admin\/games\/\$\{result\.gameId\}\/info`\)/)
  assert.match(gamesPage, /t\('admin\.button\.games\.import_data'\)/)
  assert.match(gamesPage, /t\('admin\.notification\.games\.import_data\.importing'\)/)
})

test('data transfer locale keys exist in the maintained locales', () => {
  for (const locale of ['en-US', 'zh-CN']) {
    const admin = JSON.parse(readFileSync(`src/locales/${locale}/admin.json`, 'utf8'))
    assert.equal(typeof admin.button.games.export_data, 'string', `${locale} button.games.export_data`)
    assert.equal(typeof admin.button.games.import_data, 'string', `${locale} button.games.import_data`)
    assert.equal(typeof admin.notification.games.import_data.importing, 'string', `${locale} importing`)
    const success: string = admin.notification.games.import_data.success
    for (const token of ['{{rows}}', '{{teamsCreated}}', '{{teamsMatched}}', '{{usersCreated}}', '{{usersMatched}}']) {
      assert.ok(success.includes(token), `${locale} success message is missing ${token}`)
    }
  }
})
