import { HeadlessMantineProvider } from '@mantine/core'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import api, { type TeamUpdateModel } from '../Api'
import { installTestDom } from '../test/installDom'
import { TeamCreateModal } from './TeamCreateModal'

test('guided team creation requires input and completes only after a successful submit', async () => {
  const browser = new Window({ url: 'https://rsctf.test/teams' })
  const restoreDom = installTestDom(browser)
  const i18n = i18next.createInstance()
  await i18n.init({
    lng: 'en',
    fallbackLng: 'en',
    resources: {
      en: {
        translation: {
          team: {
            button: { create: 'Create team' },
            content: { create: 'Enter your team details.' },
            label: { name: 'Team name', bio: 'Team bio' },
            placeholder: { bio: 'Optional bio' },
            notification: {
              create: {
                success: { title: 'Team created', message: '{{team}} is ready' },
              },
            },
          },
        },
      },
    },
  })
  const originalCreate = api.team.teamCreateTeam
  const teamApi = api.team as typeof api.team & { teamCreateTeam: typeof api.team.teamCreateTeam }
  let submitted: TeamUpdateModel | undefined
  const actions: string[] = []
  teamApi.teamCreateTeam = (async (model: TeamUpdateModel) => {
    submitted = model
    return { data: { name: model.name } }
  }) as typeof api.team.teamCreateTeam
  const { createRoot } = await import('react-dom/client')
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  try {
    await act(async () => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(TeamCreateModal, {
              opened: true,
              title: 'Create team',
              disallowCreate: false,
              onClose: () => actions.push('close'),
              mutate: () => actions.push('mutate'),
              onTeamReady: () => actions.push('ready'),
            })
          )
        )
      )
    })

    const form = browser.document.querySelector<HTMLFormElement>('[data-guide="team-create-workflow"]')
    const input = form?.querySelector<HTMLInputElement>('input[type="text"]')
    const submit = form?.querySelector<HTMLButtonElement>('button[type="submit"]')
    assert.ok(form)
    assert.ok(input)
    assert.ok(submit)
    assert.equal(form.hasAttribute('data-guide-interaction-scope'), true)
    assert.equal(submit.disabled, true)

    await act(async () => {
      const setValue = Object.getOwnPropertyDescriptor(browser.HTMLInputElement.prototype, 'value')?.set
      assert.ok(setValue)
      setValue.call(input, 'rookies')
      input.dispatchEvent(new browser.Event('input', { bubbles: true }))
      input.dispatchEvent(new browser.Event('change', { bubbles: true }))
    })
    assert.equal(submit.disabled, false)
    assert.deepEqual(actions, [])

    await act(async () => {
      form.dispatchEvent(new browser.Event('submit', { bubbles: true, cancelable: true }))
      await Promise.resolve()
      await Promise.resolve()
    })
    assert.equal(submitted?.name, 'rookies')
    assert.deepEqual(actions, ['ready', 'mutate', 'close'])
  } finally {
    await act(async () => root.unmount())
    teamApi.teamCreateTeam = originalCreate
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
