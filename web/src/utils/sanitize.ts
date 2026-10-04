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

const VIDEO_EMBED_ORIGINS = new Set(['https://www.youtube.com', 'https://www.youtube-nocookie.com'])
const VIDEO_EMBED_SANDBOX = 'allow-scripts allow-same-origin allow-presentation allow-popups'
const VIDEO_EMBED_PERMISSIONS = 'accelerometer; autoplay; encrypted-media; gyroscope; picture-in-picture; web-share'

const isAllowedVideoEmbed = (source: URL): boolean =>
  VIDEO_EMBED_ORIGINS.has(source.origin) &&
  source.username === '' &&
  source.password === '' &&
  source.pathname.startsWith('/embed/') &&
  source.pathname.length > '/embed/'.length

/**
 * Retain only known video-player frames, then replace every author-controlled
 * iframe attribute with one fixed least-privilege profile. In particular, an
 * event manager cannot use inline positioning to cover RSCTF controls or grant
 * a framed page forms, top-navigation, downloads, or clipboard access.
 */
const secureVideoEmbeds = (html: string): string => {
  const template = document.createElement('template')
  template.innerHTML = html

  for (const frame of template.content.querySelectorAll('iframe')) {
    const rawSource = frame.getAttribute('src')
    let source: URL
    try {
      source = new URL(rawSource ?? '', document.baseURI)
    } catch {
      frame.remove()
      continue
    }

    if (!rawSource || !isAllowedVideoEmbed(source)) {
      frame.remove()
      continue
    }

    const title = frame.getAttribute('title')?.trim() || 'Embedded video'
    for (const attribute of Array.from(frame.attributes)) frame.removeAttribute(attribute.name)

    frame.setAttribute('src', source.toString())
    frame.setAttribute('title', title)
    frame.setAttribute('width', '560')
    frame.setAttribute('height', '315')
    frame.setAttribute('loading', 'lazy')
    frame.setAttribute('referrerpolicy', 'strict-origin-when-cross-origin')
    frame.setAttribute('sandbox', VIDEO_EMBED_SANDBOX)
    frame.setAttribute('allow', VIDEO_EMBED_PERMISSIONS)
    frame.setAttribute('allowfullscreen', '')
    frame.setAttribute('data-rsctf-video-embed', '')
  }

  return template.innerHTML
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

/**
 * Sanitize block Markdown while retaining sandboxed YouTube players. Embeds
 * stay disabled in inline Markdown (hints, labels, and notices) and in syntax
 * highlighting, all of which continue to use `sanitizeMarkdownHtml`.
 */
export const sanitizeMarkdownDocumentHtml = (html: string): string => {
  const sanitized = currentPurifier().sanitize(html, {
    ADD_ATTR: [
      'target',
      'encoding',
      'src',
      'title',
      'width',
      'height',
      'loading',
      'referrerpolicy',
      'sandbox',
      'allow',
      'allowfullscreen',
    ],
    ADD_TAGS: ['semantics', 'annotation', 'iframe'],
  })
  return secureVideoEmbeds(sanitized)
}
