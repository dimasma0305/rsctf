import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { SWRConfig } from 'swr'
import api, { type AiChatLinkState } from '../Api'
import { installTestDom } from '../test/installDom'
import { AiChatLinksSection } from './AiChatLinksSection'

const baseState: AiChatLinkState = {
  editable: true,
  solved: true,
  editableUntil: 2_000_000_000_000,
  maxLinks: 2,
  providers: [
    {
      key: 'chatgpt',
      label: 'ChatGPT',
      pattern: String.raw`https://(?:chatgpt\.com|chat\.openai\.com)/share/[A-Za-z0-9-]{8,128}(?:\?[!-~]*)?`,
    },
    { key: 'claude', label: 'Claude', pattern: String.raw`https://claude\.ai/share/[A-Za-z0-9-]{8,128}(?:\?[!-~]*)?` },
  ],
  links: [],
  revision: 0,
  updatedAt: null,
  submittedBy: null,
  required: false,
  pending: false,
  declaredNoAi: false,
  solvedAt: 1_900_000_000_000,
  firstDisclosedAt: null,
  editCount: 0,
}

const flush = async () => {
  for (let index = 0; index < 10; index += 1) await Promise.resolve()
}

type SaveFn = typeof api.game.gameSaveAiChatLinks

test('AI chat links section validates, adds, saves, and reconciles conflicts', async () => {
  const browser = new Window({ url: 'https://rsctf.test/' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', fallbackLng: 'en', resources: { en: { translation: {} } } })
  const { createRoot } = await import('react-dom/client')
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  let serverState: AiChatLinkState = baseState
  let readError: unknown = null
  let reads = 0
  const gameApi = api.game as typeof api.game & { gameSaveAiChatLinks: SaveFn }
  const originalSave = gameApi.gameSaveAiChatLinks
  const saves: { links: string[]; expectedRevision: number; noAiUsed?: boolean }[] = []
  let saveResult: 'ok' | 'conflict' = 'ok'
  gameApi.gameSaveAiChatLinks = (async (_id: number, _challengeId: number, body: (typeof saves)[number]) => {
    saves.push(body)
    if (saveResult === 'conflict') throw { response: { status: 409, data: { title: 'conflict' } } }
    const disclosed = body.links.length > 0 || body.noAiUsed === true
    serverState = {
      ...serverState,
      links: body.links.map((url) => ({ url, providerKey: 'claude', providerLabel: 'Claude' })),
      revision: serverState.revision + 1,
      submittedBy: 'alice',
      declaredNoAi: body.noAiUsed === true,
      pending: serverState.required && !disclosed,
      firstDisclosedAt: disclosed ? (serverState.firstDisclosedAt ?? 1_900_000_060_000) : serverState.firstDisclosedAt,
    }
    return { status: 200, data: serverState }
  }) as unknown as SaveFn

  const pendingReports: boolean[] = []
  const mount = async (gameId = 3, challengeId = 9) => {
    const container = browser.document.createElement('div')
    browser.document.body.append(container)
    const root = createRoot(container)
    await act(async () => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(
              SWRConfig,
              {
                value: {
                  provider: () => new Map(),
                  dedupingInterval: 0,
                  fetcher: async () => {
                    reads += 1
                    if (readError) throw readError
                    return serverState
                  },
                },
              },
              createElement(AiChatLinksSection, {
                gameId,
                challengeId,
                onPendingChange: (pending: boolean) => pendingReports.push(pending),
              })
            )
          )
        )
      )
      await flush()
    })
    return { container, root }
  }

  const typeInto = async (input: HTMLInputElement, value: string) => {
    await act(async () => {
      const setValue = Object.getOwnPropertyDescriptor(browser.HTMLInputElement.prototype, 'value')?.set
      assert.ok(setValue)
      setValue.call(input, value)
      input.dispatchEvent(new browser.Event('input', { bubbles: true }))
      input.dispatchEvent(new browser.Event('change', { bubbles: true }))
    })
  }
  const click = async (element: Element | null | undefined) => {
    assert.ok(element)
    await act(async () => {
      ;(element as HTMLElement).click()
      await flush()
    })
  }
  const buttonNamed = (container: HTMLElement, name: RegExp) =>
    [...container.querySelectorAll<HTMLButtonElement>('button')].find((button) =>
      name.test(button.getAttribute('aria-label') ?? button.textContent ?? '')
    )

  let mounted = await mount()
  try {
    const { container } = mounted
    assert.equal(reads, 1, 'mounting performs exactly one read')
    const heading = container.querySelector('h3')
    assert.equal(heading?.textContent, 'AI chat links')
    assert.equal(container.querySelector('section')?.getAttribute('aria-labelledby'), heading?.id)
    assert.equal(container.querySelector('[data-autofocus], [autofocus]'), null, 'the section never takes focus')
    assert.equal(container.querySelector('input'), null, 'the section starts collapsed')

    const toggle = buttonNamed(container, /^Manage/)
    assert.equal(toggle?.getAttribute('aria-expanded'), 'false')
    await click(toggle)
    assert.equal(toggle?.getAttribute('aria-expanded'), 'true')
    assert.match(container.textContent ?? '', /Supported: ChatGPT, Claude/)

    const input = container.querySelector<HTMLInputElement>('input')
    assert.ok(input)
    const label = container.querySelector(`label[for="${input.id}"]`)
    assert.equal(label?.textContent?.replace(/\s+/g, ''), 'Sharelink')
    const described = container.querySelector(`[id="${input.getAttribute('aria-describedby')}"]`)
    const statusRegion = container.querySelector('[data-ai-chat-status]')
    assert.equal(statusRegion?.getAttribute('role'), 'status')
    assert.ok(described?.contains(statusRegion), 'the live verdict is the field description')
    const addButton = () => buttonNamed(container, /^Add$/)
    assert.equal(addButton()?.disabled, true)

    await typeInto(input, 'http://chatgpt.com/share/abcdefgh1234')
    assert.match(statusRegion?.textContent ?? '', /Only https:\/\/ links are accepted/)
    assert.equal(input.getAttribute('aria-invalid'), 'true')
    assert.equal(addButton()?.disabled, true)

    await typeInto(input, 'https://chatgpt.com.evil.test/share/abcdefgh1234')
    assert.match(statusRegion?.textContent ?? '', /This AI provider is not accepted/)
    assert.equal(addButton()?.disabled, true)

    await typeInto(input, 'https://CLAUDE.ai/share/abcdefgh-1234#frag')
    assert.match(statusRegion?.textContent ?? '', /Matches Claude/)
    assert.equal(input.getAttribute('aria-invalid'), null)
    assert.equal(addButton()?.disabled, false)
    await click(addButton())
    assert.equal(input.value, '')
    assert.match(container.textContent ?? '', /Not saved/)
    assert.match(container.textContent ?? '', /claude\.ai/)
    const open = container.querySelector<HTMLAnchorElement>('a[aria-label="Open link in a new tab"]')
    assert.equal(open?.getAttribute('href'), 'https://claude.ai/share/abcdefgh-1234')
    assert.equal(open?.getAttribute('target'), '_blank')
    assert.equal(open?.getAttribute('rel'), 'noopener noreferrer nofollow')
    assert.ok(buttonNamed(container, /^Copy link$/))
    assert.ok(buttonNamed(container, /^Remove link$/))

    await typeInto(input, 'https://claude.ai/share/abcdefgh-1234')
    assert.match(statusRegion?.textContent ?? '', /already in the list/)
    assert.equal(addButton()?.disabled, true)

    await typeInto(input, 'https://chatgpt.com/share/abcdefgh1234')
    await click(addButton())
    await typeInto(input, 'https://chatgpt.com/share/zzzzzzzz9999')
    assert.match(statusRegion?.textContent ?? '', /up to 2 links/)
    assert.equal(addButton()?.disabled, true)
    await click(buttonNamed(container, /^Remove link$/))

    await click(buttonNamed(container, /^Save links$/))
    assert.deepEqual(saves.at(-1), {
      links: ['https://chatgpt.com/share/abcdefgh1234'],
      expectedRevision: 0,
      noAiUsed: false,
    })
    assert.doesNotMatch(container.textContent ?? '', /Not saved/)
    assert.match(container.textContent ?? '', /Last saved by alice/)
    assert.equal(buttonNamed(container, /^Save links$/)?.disabled, true, 'a saved list is not dirty')

    // A teammate saved first: the 409 refetches and replaces the stale draft.
    saveResult = 'conflict'
    serverState = { ...serverState, revision: 5, links: [], submittedBy: 'bob' }
    const readsBeforeConflict = reads
    await click(buttonNamed(container, /^Remove link$/))
    await typeInto(input, 'https://claude.ai/share/fresh-draft-1')
    await click(addButton())
    await click(buttonNamed(container, /^Save links$/))
    assert.equal(saves.at(-1)?.expectedRevision, 1)
    assert.equal(reads, readsBeforeConflict + 1, 'a conflict performs one fresh read')
    assert.match(container.textContent ?? '', /No links attached yet/)
    assert.match(container.textContent ?? '', /Last saved by bob/)

    // Closed window: links stay readable, editing controls disappear.
    await act(async () => mounted.root.unmount())
    serverState = {
      ...baseState,
      editable: false,
      links: [{ url: 'https://claude.ai/share/abcdefgh-1234', providerKey: 'claude', providerLabel: 'Claude' }],
      revision: 2,
    }
    mounted = await mount(3, 10)
    await click(buttonNamed(mounted.container, /^Show$/))
    assert.match(mounted.container.textContent ?? '', /window for changing AI chat links has closed/)
    assert.equal(mounted.container.querySelector('input'), null)
    assert.equal(buttonNamed(mounted.container, /^Remove link$/), undefined)
    assert.ok(mounted.container.querySelector('a[aria-label="Open link in a new tab"]'))

    assert.equal(pendingReports.includes(true), false, 'an optional disclosure never gates closing')

    // Required and pending: opens expanded with a text notice and gates closing
    // until the "No AI used" declaration is confirmed.
    await act(async () => mounted.root.unmount())
    pendingReports.length = 0
    saveResult = 'ok'
    serverState = { ...baseState, required: true, pending: true, revision: 0 }
    mounted = await mount(3, 12)
    const pendingNotice = mounted.container.querySelector('[data-ai-chat-pending]')
    assert.match(pendingNotice?.textContent ?? '', /Disclosure required/)
    assert.equal(pendingNotice?.getAttribute('tabindex'), '-1', 'a blocked close can focus the notice')
    assert.equal(buttonNamed(mounted.container, /^Hide$/)?.getAttribute('aria-expanded'), 'true')
    assert.match(mounted.container.textContent ?? '', /Solved at/)
    assert.equal(pendingReports.at(-1), true)
    await click(buttonNamed(mounted.container, /^We did not use AI$/))
    assert.match(mounted.container.textContent ?? '', /Organizers can see this declaration/)
    const confirm = buttonNamed(mounted.container, /^Confirm: no AI used$/)
    assert.equal(browser.document.activeElement, confirm, 'the confirm step takes focus')
    await click(confirm)
    assert.deepEqual(saves.at(-1), { links: [], expectedRevision: 0, noAiUsed: true })
    assert.equal(pendingReports.at(-1), false, 'the declaration releases the dialog')
    assert.equal(mounted.container.querySelector('[data-ai-chat-pending]'), null)
    assert.ok(mounted.container.querySelector('[data-ai-chat-declared]'), 'declared: ' + mounted.container.textContent)
    assert.match(mounted.container.textContent ?? '', /Declared: no AI used/)
    assert.equal(buttonNamed(mounted.container, /^We did not use AI$/), undefined)

    // Adding a link while declared warns that it replaces the declaration.
    const declaredInput = mounted.container.querySelector<HTMLInputElement>('input')
    assert.ok(declaredInput, 'input: ' + mounted.container.textContent)
    await typeInto(declaredInput, 'https://claude.ai/share/replace-declaration')
    await click(buttonNamed(mounted.container, /^Add$/))
    assert.match(
      mounted.container.querySelector('[data-ai-chat-save-warning]')?.textContent ?? '',
      /replaces the "No AI used" declaration/
    )
    await click(buttonNamed(mounted.container, /^Save links$/))
    assert.deepEqual(saves.at(-1), {
      links: ['https://claude.ai/share/replace-declaration'],
      expectedRevision: 1,
      noAiUsed: false,
    })

    // Clearing a required disclosure warns that the challenge becomes pending again.
    await click(buttonNamed(mounted.container, /^Remove link$/))
    assert.match(
      mounted.container.querySelector('[data-ai-chat-save-warning]')?.textContent ?? '',
      /will need a disclosure again/
    )

    // Pending after the edit window closed: shown read-only, never gating.
    await act(async () => mounted.root.unmount())
    pendingReports.length = 0
    serverState = { ...baseState, required: true, pending: true, editable: false }
    mounted = await mount(3, 13)
    assert.equal(pendingReports.includes(true), false)

    // A disabled event or a non-Jeopardy challenge renders nothing and never gates.
    await act(async () => mounted.root.unmount())
    pendingReports.length = 0
    readError = { response: { status: 404 }, status: 404 }
    mounted = await mount(3, 11)
    assert.equal(mounted.container.textContent, '')
    assert.equal(pendingReports.includes(true), false)
  } finally {
    await act(async () => mounted.root.unmount())
    gameApi.gameSaveAiChatLinks = originalSave
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
