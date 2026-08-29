import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import {
  guideTargetAcceptsKeyboardEntry,
  guideTargetHasKeyboardEntryFocus,
  guideTargetKeyboardActivation,
  guideTargetMatchesActivation,
} from './GuideSpotlightModal'

test('keyboard focus activates text-entry guide targets but not action buttons', async () => {
  const browser = new Window({ url: 'https://rsctf.test/teams' })
  const input = browser.document.createElement('input')
  input.type = 'text'
  const textarea = browser.document.createElement('textarea')
  const select = browser.document.createElement('select')
  const button = browser.document.createElement('button')
  const submit = browser.document.createElement('input')
  submit.type = 'submit'
  input.dataset.guide = 'team-create-name'

  assert.equal(guideTargetAcceptsKeyboardEntry(input as unknown as HTMLElement), true)
  assert.equal(guideTargetAcceptsKeyboardEntry(textarea as unknown as HTMLElement), true)
  assert.equal(guideTargetAcceptsKeyboardEntry(select as unknown as HTMLElement), true)
  assert.equal(guideTargetAcceptsKeyboardEntry(button as unknown as HTMLElement), false)
  assert.equal(guideTargetAcceptsKeyboardEntry(submit as unknown as HTMLElement), false)

  browser.document.body.append(input, textarea, select, button, submit)
  input.focus()
  assert.equal(
    guideTargetHasKeyboardEntryFocus(
      input as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    true
  )
  assert.equal(
    guideTargetHasKeyboardEntryFocus(
      textarea as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    false
  )
  assert.equal(
    guideTargetHasKeyboardEntryFocus(
      button as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    false
  )
  assert.equal(
    guideTargetKeyboardActivation(
      input as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    'team-create-name'
  )

  button.focus()
  assert.equal(
    guideTargetKeyboardActivation(
      input as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    undefined
  )

  await browser.happyDOM.close()
})

test('segmented guide targets advance on the requested radio action, not focus alone', async () => {
  const browser = new Window({ url: 'https://rsctf.test/challenges' })
  const target = browser.document.createElement('div')
  target.dataset.guide = 'wsrx-local-mode'
  target.dataset.guideValue = 'wsrx'

  const localInput = browser.document.createElement('input')
  localInput.type = 'radio'
  localInput.id = 'proxy-mode-wsrx'
  localInput.value = 'wsrx'
  const localLabel = browser.document.createElement('label')
  localLabel.htmlFor = localInput.id
  const localText = browser.document.createElement('span')
  localLabel.append(localText)

  const wssInput = browser.document.createElement('input')
  wssInput.type = 'radio'
  wssInput.id = 'proxy-mode-wss'
  wssInput.value = 'wss'
  const wssLabel = browser.document.createElement('label')
  wssLabel.htmlFor = wssInput.id
  const wssText = browser.document.createElement('span')
  wssLabel.append(wssText)

  target.append(localInput, localLabel, wssInput, wssLabel)
  browser.document.body.append(target)

  localInput.focus()
  assert.equal(
    guideTargetKeyboardActivation(
      target as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    undefined
  )
  assert.equal(guideTargetMatchesActivation(target as unknown as HTMLElement, localInput), true)
  assert.equal(guideTargetMatchesActivation(target as unknown as HTMLElement, localText), true)
  assert.equal(guideTargetMatchesActivation(target as unknown as HTMLElement, wssInput), false)
  assert.equal(guideTargetMatchesActivation(target as unknown as HTMLElement, wssText), false)

  await browser.happyDOM.close()
})
