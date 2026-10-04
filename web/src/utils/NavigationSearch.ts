interface SearchablePage {
  title: string
  section: string
  link: string
  keywords: string
}

const normalize = (value: string) => value.normalize('NFKD').replace(/\p{M}/gu, '').toLocaleLowerCase().trim()

/** Search only the caller's permitted destinations; preserve workspace order on ties. */
export function searchNavigation<T extends SearchablePage>(pages: readonly T[], query: string): T[] {
  const normalized = normalize(query)
  const words = normalized.split(/\s+/).filter(Boolean)
  if (!words.length) return [...pages]

  const rank = (page: T) => {
    const title = normalize(page.title)
    if (title === normalized) return 0
    if (title.startsWith(normalized)) return 1
    if (words.every((word) => title.includes(word))) return 2
    return 3
  }

  return pages
    .filter((page) => {
      const text = normalize(`${page.title} ${page.section} ${page.link} ${page.keywords}`)
      return words.every((word) => text.includes(word))
    })
    .sort((a, b) => rank(a) - rank(b))
}
