import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import { installTestDom } from '../test/installDom'
import { sanitizeMarkdownDocumentHtml, sanitizeMarkdownHtml } from './sanitize'

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

test('block Markdown keeps only sandboxed HTTPS YouTube embeds', async () => {
  const browser = new Window({ url: 'https://ctf.example/challenges' })
  const restoreDom = installTestDom(browser)
  try {
    const sanitized = sanitizeMarkdownDocumentHtml(`
      <iframe
        src="https://www.youtube.com/embed/video-id?start=5"
        title="Challenge walkthrough"
        style="position:fixed;inset:0"
        onload="alert(1)"
        sandbox="allow-forms allow-top-navigation"
        allow="camera; microphone; clipboard-write"
      ></iframe>
      <iframe src="https://www.youtube.com.evil.example/embed/video-id"></iframe>
      <iframe src="https://www.youtube.com/watch?v=video-id"></iframe>
      <iframe src="javascript:alert(1)"></iframe>
    `)

    const container = browser.document.createElement('div')
    container.innerHTML = sanitized
    const frames = container.querySelectorAll('iframe')
    assert.equal(frames.length, 1)

    const frame = frames[0]
    assert.equal(frame.getAttribute('src'), 'https://www.youtube.com/embed/video-id?start=5')
    assert.equal(frame.getAttribute('title'), 'Challenge walkthrough')
    assert.equal(frame.getAttribute('loading'), 'lazy')
    assert.equal(frame.getAttribute('referrerpolicy'), 'strict-origin-when-cross-origin')
    assert.equal(frame.getAttribute('sandbox'), 'allow-scripts allow-same-origin allow-presentation allow-popups')
    assert.equal(
      frame.getAttribute('allow'),
      'accelerometer; autoplay; encrypted-media; gyroscope; picture-in-picture; web-share'
    )
    assert.equal(frame.getAttribute('style'), null)
    assert.equal(frame.getAttribute('onload'), null)
    assert.equal(frame.hasAttribute('allowfullscreen'), true)
    assert.equal(frame.hasAttribute('data-rsctf-video-embed'), true)
  } finally {
    await browser.happyDOM.close()
    restoreDom()
  }
})

test('inline Markdown continues to strip every iframe', async () => {
  const browser = new Window({ url: 'https://ctf.example/challenges' })
  const restoreDom = installTestDom(browser)
  try {
    assert.equal(sanitizeMarkdownHtml('<iframe src="https://www.youtube.com/embed/video-id"></iframe>'), '')
  } finally {
    await browser.happyDOM.close()
    restoreDom()
  }
})
