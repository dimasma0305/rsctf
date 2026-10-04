import { HeadlessMantineProvider } from '@mantine/core'
import dayjs from 'dayjs'
import { Window } from 'happy-dom'
import i18next from 'i18next'
import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { I18nextProvider } from 'react-i18next'
import { MemoryRouter } from 'react-router'
import { installTestDom } from '../../test/installDom'

test('cached schedule corrects its marker when the live server clock replaces browser skew', async (context) => {
  const browser = new Window({ url: 'https://rsctf.test/games' })
  const restoreDom = installTestDom(browser)
  const localNow = Date.UTC(2033, 4, 17, 18, 45, 10)
  const serverNow = localNow - 2 * 60 * 60_000
  context.mock.timers.enable({
    apis: ['Date', 'setInterval', 'setTimeout'],
    now: new Date(localNow),
  })

  const { GanttTimeLine } = await import('./GanttTimeline')
  const { LanguageProvider } = await import('../../utils/I18n')
  const { observeServerTime, serverClockTestApi } = await import('../../utils/ServerClock')
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en-US', fallbackLng: 'en', resources: { en: { translation: {} } } })
  const container = browser.document.createElement('div')
  browser.document.body.append(container)
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(container)
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

  const markerPosition = () => {
    const canvas = container.querySelector<HTMLElement>('[style*="--today-position"]')
    assert.ok(canvas)
    return Number.parseFloat(canvas.style.getPropertyValue('--today-position'))
  }

  try {
    serverClockTestApi.reset()
    await act(async () => {
      root.render(
        createElement(
          HeadlessMantineProvider,
          null,
          createElement(
            I18nextProvider,
            { i18n },
            createElement(
              LanguageProvider,
              null,
              createElement(
                MemoryRouter,
                null,
                createElement(GanttTimeLine, {
                  // This represents an event list restored by the persistent
                  // SWR provider before its revalidation response arrives.
                  items: [
                    {
                      id: 17,
                      textTitle: 'Cached event',
                      title: 'Cached event',
                      start: dayjs(serverNow - 60 * 60_000),
                      end: dayjs(serverNow + 60 * 60_000),
                    },
                  ],
                })
              )
            )
          )
        )
      )
    })

    const browserClockPosition = markerPosition()
    await act(async () => {
      assert.equal(observeServerTime(serverNow, localNow), true)
    })
    const correctedPosition = markerPosition()
    assert.ok(
      browserClockPosition - correctedPosition > 0.1,
      'a same-day offset correction must replace the marker derived from the browser clock'
    )

    // The grid and marker use a one-minute authoritative-clock bucket: shared
    // one-second ticks do not rebuild the model, while the next bucket does.
    await act(async () => context.mock.timers.tick(30_000))
    assert.equal(markerPosition(), correctedPosition)
    await act(async () => context.mock.timers.tick(30_000))
    assert.ok(markerPosition() > correctedPosition)
  } finally {
    await act(async () => root.unmount())
    serverClockTestApi.reset()
    context.mock.timers.reset()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    await browser.happyDOM.close()
    restoreDom()
  }
})
