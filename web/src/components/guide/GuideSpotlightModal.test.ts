import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import {
  guideTargetAcceptsKeyboardEntry,
  guideTargetHasKeyboardEntryFocus,
  guideTargetKeyboardActivation,
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

test('keyboard focus on a label-associated control activates its visible guide target', async () => {
  const browser = new Window({ url: 'https://rsctf.test/challenges' })
  const input = browser.document.createElement('input')
  input.type = 'radio'
  input.id = 'proxy-mode-wsrx'
  const label = browser.document.createElement('label')
  label.htmlFor = input.id
  const target = browser.document.createElement('span')
  target.dataset.guide = 'wsrx-local-mode'
  label.append(target)
  browser.document.body.append(input, label)

  input.focus()
  assert.equal(
    guideTargetKeyboardActivation(
      target as unknown as HTMLElement,
      browser.document.activeElement as unknown as Element
    ),
    'wsrx-local-mode'
  )

  await browser.happyDOM.close()
})
