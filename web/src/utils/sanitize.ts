import createDOMPurify, { type DOMPurify, type WindowLike } from 'dompurify'

let purifier: DOMPurify | null = null
let purifierWindow: WindowLike | null = null

/**
 * DOMPurify's default export is created at module evaluation. In an isolated
 * test/SSR import there may not be a DOM yet, so that export remains only a
 * factory and has no `sanitize` method. Bind lazily to the current window and
 * rebind when a test replaces it; the browser path keeps one stable instance.
 */
const currentPurifier = (): DOMPurify => {
  if (typeof window === 'undefined') {
    throw new Error('Markdown sanitization requires a browser DOM')
  }
  const currentWindow = window as unknown as WindowLike
  if (purifier === null || purifierWindow !== currentWindow) {
    const nextPurifier = createDOMPurify(currentWindow)
    if (!nextPurifier.isSupported || typeof nextPurifier.sanitize !== 'function') {
      throw new Error('Markdown sanitization is unavailable in this browser')
    }
    purifier = nextPurifier
    purifierWindow = currentWindow
  }
  return purifier
}

/**
 * Sanitize rendered-Markdown HTML before injecting it via
 * `dangerouslySetInnerHTML`. Strips `<script>`, event-handler attributes
 * (`onerror`, `onclick`, …) and dangerous URL schemes (`javascript:`, `data:`)
 * while preserving the markup our renderers legitimately emit — KaTeX math
 * (HTML + MathML) and Shiki syntax highlighting (styled `<span>`s).
 *
 * Markdown fields (challenge content/hints, posts, game notices, footer) are
 * editable by game organizers (the EventManager role), which is lower-trust
 * than the platform admin — so this output must not be treated as safe HTML.
 * DOMPurify's default profile keeps HTML + SVG + MathML; we only additionally
 * allow `target` so external links can still open in a new tab.
 */
export const sanitizeMarkdownHtml = (html: string): string =>
  currentPurifier().sanitize(html, {
    // `target` for new-tab links; `semantics`/`annotation` (+ its `encoding`) are
    // KaTeX's inert MathML a11y layer — keep them so screen readers / copy-as-TeX
    // still work. All are non-scripting; XSS vectors stay stripped by default.
    ADD_ATTR: ['target', 'encoding'],
    ADD_TAGS: ['semantics', 'annotation'],
  })
