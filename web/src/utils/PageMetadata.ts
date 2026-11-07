export const DEFAULT_SITE_DESCRIPTION =
  'Practice cybersecurity, join capture-the-flag competitions, and learn with your team.'

export const isIndexablePage = (path: string, confirmedPublic = false) => {
  const normalized = path.replace(/\/+$/, '') || '/'
  return (
    ['/', '/games', '/posts', '/about', '/guide'].includes(normalized) ||
    (confirmedPublic && /^\/(games|posts)\/[^/]+$/.test(normalized))
  )
}

export interface PageMetadata {
  title: string
  description?: string | null
  imageUrl?: string | null
  publicUrl?: string | null
  path: string
  indexable: boolean
}

export const updatePageMetadata = (metadata: PageMetadata, document: Document, browserOrigin: string) => {
  const meta = (attribute: 'name' | 'property', name: string, value?: string) => {
    let element = document.head.querySelector<HTMLMetaElement>(`meta[${attribute}="${name}"]`)
    if (value === undefined) {
      element?.remove()
      return
    }
    if (!element) {
      element = document.createElement('meta')
      element.setAttribute(attribute, name)
      document.head.append(element)
    }
    element.content = value
  }
  const description = (metadata.description?.trim() || DEFAULT_SITE_DESCRIPTION).replace(/\s+/g, ' ').slice(0, 320)
  let origin: string | undefined
  try {
    const configured = new URL(metadata.publicUrl || browserOrigin)
    if (['http:', 'https:'].includes(configured.protocol) && !configured.username && !configured.password) {
      origin = configured.origin
    }
  } catch {
    /* Omit absolute metadata if the deployment URL is invalid. */
  }
  const canonical = origin && metadata.indexable ? `${origin}${metadata.path.replace(/\/+$/, '') || '/'}` : undefined
  let link = document.head.querySelector<HTMLLinkElement>('link[rel="canonical"]')
  if (canonical) {
    if (!link) {
      link = document.createElement('link')
      link.rel = 'canonical'
      document.head.append(link)
    }
    link.href = canonical
  } else link?.remove()
  meta('name', 'description', description)
  meta('name', 'robots', metadata.indexable ? 'index, follow' : 'noindex, nofollow')
  meta('property', 'og:type', 'website')
  meta('property', 'og:title', metadata.title)
  meta('property', 'og:description', description)
  meta('property', 'og:url', canonical)
  meta('name', 'twitter:card', 'summary')
  meta('name', 'twitter:title', metadata.title)
  meta('name', 'twitter:description', description)
  let image: string | undefined
  try {
    const url = new URL(metadata.imageUrl || '/favicon.png', origin)
    if (['http:', 'https:'].includes(url.protocol)) image = url.href
  } catch {
    /* No public origin is configured. */
  }
  meta('property', 'og:image', image)
  meta('name', 'twitter:image', image)
}
