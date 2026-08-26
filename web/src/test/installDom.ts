import { Window } from 'happy-dom'

/** Install one happy-dom window for a mounted component test and restore the
 * process globals afterward. Keeping this in one place prevents subtle DOM
 * differences between timer and interaction regressions. */
export const installTestDom = (browser: Window) => {
  if (!browser.document.doctype) {
    browser.document.insertBefore(
      browser.document.implementation.createDocumentType('html', '', ''),
      browser.document.documentElement
    )
  }
  // happy-dom does not currently expose Document.compatMode. Browser code
  // (notably KaTeX) uses the standards-mode value to reject quirks rendering.
  Object.defineProperty(browser.document, 'compatMode', { configurable: true, value: 'CSS1Compat' })
  const values: Record<string, unknown> = {
    window: browser,
    document: browser.document,
    navigator: browser.navigator,
    Node: browser.Node,
    Element: browser.Element,
    HTMLElement: browser.HTMLElement,
    HTMLIFrameElement: browser.HTMLIFrameElement,
    SVGElement: browser.SVGElement,
    MutationObserver: browser.MutationObserver,
    Event: browser.Event,
    CustomEvent: browser.CustomEvent,
    StorageEvent: browser.StorageEvent,
    MouseEvent: browser.MouseEvent,
    File: browser.File,
    getComputedStyle: browser.getComputedStyle.bind(browser),
    requestAnimationFrame: browser.requestAnimationFrame.bind(browser),
    cancelAnimationFrame: browser.cancelAnimationFrame.bind(browser),
  }
  const previous = new Map<string, PropertyDescriptor | undefined>()

  for (const [name, value] of Object.entries(values)) {
    previous.set(name, Object.getOwnPropertyDescriptor(globalThis, name))
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value })
  }

  return () => {
    for (const [name, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor)
      else delete (globalThis as Record<string, unknown>)[name]
    }
  }
}
