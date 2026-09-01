import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { installTestDom } from '../test/installDom'
import { sanitizeMarkdownHtml } from './sanitize'

const sanitizeWithInstalledWindow = async (browser: Window) => {
  const restoreDom = installTestDom(browser)
  try {
    // Happy DOM is sufficient to prove late factory binding, but it is not a
    // standards-complete security oracle for DOMPurify's browser parser.
    assert.equal(sanitizeMarkdownHtml('safe'), 'safe')
  } finally {
    await browser.happyDOM.close()
    restoreDom()
  }
}

test('Markdown sanitizer initializes after an early DOM-less import and rebinds to a replaced window', async () => {
  assert.throws(() => sanitizeMarkdownHtml('safe'), /requires a browser DOM/)
  await sanitizeWithInstalledWindow(new Window({ url: 'https://first.rsctf.test/' }))
  await sanitizeWithInstalledWindow(new Window({ url: 'https://second.rsctf.test/' }))
})
