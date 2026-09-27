import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
import test from 'node:test'

const api = readFileSync('src/Api.ts', 'utf8')
const section = readFileSync('src/components/AiChatLinksSection.tsx', 'utf8')
const challengeModal = readFileSync('src/components/ChallengeModal.tsx', 'utf8')
const gameChallengeModal = readFileSync('src/components/GameChallengeModal.tsx', 'utf8')
const challengePanel = readFileSync('src/components/ChallengePanel.tsx', 'utf8')
const providers = readFileSync('src/components/admin/AiChatProvidersSettings.tsx', 'utf8')
const settings = readFileSync('src/pages/admin/Settings.tsx', 'utf8')
const navigation = readFileSync('src/components/admin/navigation.ts', 'utf8')
const infoPage = readFileSync('src/pages/admin/games/[id]/Info.tsx', 'utf8')
const monitorTabs = readFileSync('src/components/WithGameMonitor.tsx', 'utf8')
const monitorPagePath = 'src/pages/games/[id]/monitor/ai-chats.tsx'
const monitorPage = readFileSync(monitorPagePath, 'utf8')

const contractSection = (start: string, end: string) => {
  const startIndex = api.indexOf(start)
  const endIndex = api.indexOf(end, startIndex + start.length)
  assert.notEqual(startIndex, -1, `missing API contract start: ${start}`)
  assert.notEqual(endIndex, -1, `missing API contract end: ${end}`)
  return api.slice(startIndex, endIndex)
}

test('player and monitor AI chat routes use the fixed paths and methods', () => {
  const get = contractSection('gameGetAiChatLinks: (', 'useGameGetAiChatLinks: (')
  assert.match(get, /this\.request<AiChatLinkState, RequestResponse>/)
  assert.match(get, /path: `\/api\/game\/\$\{id\}\/challenges\/\$\{challengeId\}\/ai-chats`/)
  assert.match(get, /method: "GET"/)

  const save = contractSection('gameSaveAiChatLinks: (', 'gameListAiChatLinks: (')
  assert.match(save, /data: AiChatLinkUpdateModel/)
  assert.match(save, /path: `\/api\/game\/\$\{id\}\/challenges\/\$\{challengeId\}\/ai-chats`/)
  assert.match(save, /method: "PUT"/)
  assert.match(save, /type: ContentType\.Json/)

  const list = contractSection('gameListAiChatLinks: (', 'useGameListAiChatLinks: (')
  assert.match(list, /this\.request<AiChatLinkRecordPage, RequestResponse>/)
  assert.match(list, /path: `\/api\/game\/\$\{id\}\/ai-chats`/)
  assert.match(list, /method: "GET"/)
  assert.match(list, /count\?: number;[\s\S]*skip\?: number;[\s\S]*challengeId\?: number;/)

  assert.match(
    api,
    /export interface AiChatLinkUpdateModel \{[\s\S]*links: string\[\];[\s\S]*expectedRevision: number;/
  )
  assert.match(
    api,
    /export interface AiChatLinkState \{[\s\S]*editable: boolean;[\s\S]*editableUntil: number;[\s\S]*maxLinks: number;[\s\S]*providers: AiChatProviderRule\[\];[\s\S]*links: AiChatLink\[\];[\s\S]*revision: number;[\s\S]*updatedAt: number \| null;[\s\S]*submittedBy: string \| null;/
  )
  assert.match(api, /export interface AiChatLinkRecordLink extends AiChatLink \{[\s\S]*providerActive: boolean;/)
  assert.match(api, /export interface AiChatLinkRecordPage \{[\s\S]*total: number;[\s\S]*items: AiChatLinkRecord\[\];/)
})

test('admin AI provider routes use the fixed paths and methods', () => {
  const list = contractSection('adminGetAiChatProviders: (', 'useAdminGetAiChatProviders: (')
  assert.match(list, /this\.request<AiChatProviderListModel, RequestResponse>/)
  assert.match(list, /path: `\/api\/admin\/ai-chat-providers`/)
  assert.match(list, /method: "GET"/)

  const save = contractSection('adminSaveAiChatProvider: (', 'adminDeleteAiChatProvider: (')
  assert.match(save, /data: AiChatProviderUpdateModel/)
  assert.match(save, /path: `\/api\/admin\/ai-chat-providers\/\$\{encodeURIComponent\(key\)\}`/)
  assert.match(save, /method: "PUT"/)

  const remove = contractSection('adminDeleteAiChatProvider: (', 'adminGetConfigs: (')
  assert.match(remove, /path: `\/api\/admin\/ai-chat-providers\/\$\{encodeURIComponent\(key\)\}`/)
  assert.match(remove, /method: "DELETE"/)

  assert.match(
    api,
    /export interface AiChatProviderModel \{[\s\S]*builtin: boolean;[\s\S]*enabled: boolean;[\s\S]*examples: string\[\];/
  )
  assert.match(api, /export interface AiChatProviderListModel \{[\s\S]*maxCustomProviders: number;/)
})

test('the event switch is exposed on both game models and the admin Info page', () => {
  assert.match(
    contractSection('export interface GameInfoModel {', 'export interface GamePurgeModel'),
    /aiChatLinksEnabled\?: boolean;/
  )
  assert.match(
    contractSection('export interface DetailedGameInfoModel {', 'export interface DivisionInfo'),
    /aiChatLinksEnabled\?: boolean;/
  )
  assert.match(infoPage, /checked=\{game\?\.aiChatLinksEnabled \?\? false\}/)
  assert.match(infoPage, /setGame\(\{ \.\.\.game, aiChatLinksEnabled: e\.target\.checked \}\)/)
})

test('the player section is Jeopardy-only, caller-owned, and never gates the modal', () => {
  // ChallengeModal is shared by preview/practice callers: it renders caller
  // content only and never fetches AI chat state itself.
  assert.match(challengeModal, /solvedExtras\?: ReactNode/)
  assert.match(challengeModal, /\{reviewSection\}\s*\{solved && solvedExtras\}/)
  assert.doesNotMatch(challengeModal, /AiChat|ai-chats/)

  assert.match(challengePanel, /aiChatLinksEnabled=\{game\?\.aiChatLinksEnabled === true\}/)
  assert.match(gameChallengeModal, /aiChatLinksEnabled = false/)
  for (const type of ['StaticAttachment', 'StaticContainer', 'DynamicAttachment', 'DynamicContainer']) {
    assert.match(gameChallengeModal, new RegExp(`ChallengeType\\.${type},`))
  }
  assert.doesNotMatch(
    gameChallengeModal.slice(gameChallengeModal.indexOf('const AI_CHAT_LINK_TYPES'), gameChallengeModal.indexOf('])')),
    /AttackDefense|KingOfTheHill/
  )
  // Keyed by challenge so an inline-pane switch cannot carry a draft across.
  assert.match(gameChallengeModal, /<AiChatLinksSection key=\{`\$\{gameId\}:\$\{challengeId\}`\}/)
  assert.match(gameChallengeModal, /solvedExtras=\{aiChatLinks\}/)

  // One non-polled read; the section must not take focus from the review flow.
  assert.match(section, /refreshInterval: 0/)
  assert.match(section, /revalidateOnFocus: false/)
  assert.doesNotMatch(section, /autoFocus|data-autofocus|onClose/)
  assert.match(section, /expectedRevision: state\.revision/)
  assert.match(section, /httpErrorStatus\(saveError\) === 409/)
})

test('rendered AI chat links are guarded, isolated, and named', () => {
  for (const source of [section, monitorPage]) {
    assert.match(source, /const href = safeAiChatHref\(/)
    assert.match(source, /rel="noopener noreferrer nofollow"/)
    assert.match(source, /target="_blank"/)
    assert.doesNotMatch(source, /href=\{(?:url|link\.url)\}/)
  }
  // Mantine owns aria-describedby: the live verdict lives in the description slot.
  assert.match(section, /description=\{\s*<span role="status" aria-live="polite"/)
  assert.doesNotMatch(section, /aria-describedby=/)
})

test('admin settings and monitor tabs register the AI chat surfaces', () => {
  assert.match(navigation, /\{ key: 'ai_links', icon: mdiRobotOutline/)
  assert.match(settings, /ai_links: aiLinksConfigured \? 'configured' : 'inactive'/)
  assert.match(settings, /activeSection === 'ai_links' && <AiChatProvidersSettings \/>/)
  assert.match(providers, /provider\.builtin \? \{ enabled \}/)
  assert.match(providers, /modals\.openConfirmModal\(/)

  // vite-plugin-pages derives the route from the file name.
  assert.ok(existsSync(monitorPagePath))
  assert.match(monitorTabs, /path: 'ai-chats'/)
  assert.match(monitorPage, /count: ITEM_COUNT_PER_PAGE/)
  assert.match(monitorPage, /const ITEM_COUNT_PER_PAGE = 50/)
})

test('every AI chat locale key exists in the maintained locales', () => {
  const sources = [section, providers, monitorPage, monitorTabs, infoPage]
  const keys = new Set<string>(['admin.content.settings.nav.ai_links'])
  for (const source of sources) {
    for (const match of source.matchAll(/t\(\s*'([a-z_.]+)'/g)) {
      if (/ai_chat|ai_links/.test(match[1])) keys.add(match[1])
    }
  }
  assert.ok(keys.size > 90, `expected the AI chat key catalog, found ${keys.size}`)
  for (const locale of ['en-US', 'zh-CN']) {
    const namespaces: Record<string, unknown> = {}
    for (const namespace of ['admin', 'challenge', 'game']) {
      namespaces[namespace] = JSON.parse(readFileSync(`src/locales/${locale}/${namespace}.json`, 'utf8'))
    }
    for (const key of keys) {
      const [namespace, ...path] = key.split('.')
      const value = path.reduce<unknown>(
        (node, part) => (node && typeof node === 'object' ? (node as Record<string, unknown>)[part] : undefined),
        namespaces[namespace]
      )
      assert.equal(typeof value, 'string', `${locale} is missing ${key}`)
    }
  }
})
