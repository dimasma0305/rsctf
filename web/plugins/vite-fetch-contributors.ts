import { Plugin } from 'vite'

const projectOwner = {
  login: 'dimasma0305',
  html_url: 'https://github.com/dimasma0305',
  avatar_url: 'https://github.com/dimasma0305.png',
  contributions: 1,
}

let contributorsData: any[] = [projectOwner]

export function fetchContributors(): Plugin {
  return {
    name: 'fetch-contributors',
    async buildStart() {
      try {
        const response = await fetch('https://api.github.com/repos/dimasma0305/rsctf/contributors')
        if (!response.ok) {
          console.warn('Failed to fetch contributors from GitHub')
          return
        }
        const contributors = (await response.json()) as any[]
        const projectContributors = contributors
          .filter((c) => !c.login.includes('bot') && c.contributions)
          .map((c: any) => ({
            login: c.login,
            html_url: c.html_url,
            avatar_url: c.avatar_url,
            contributions: c.contributions,
          }))
        contributorsData = projectContributors.length ? projectContributors : [projectOwner]
        console.log('Contributors data fetched')
      } catch (error) {
        console.warn('Error fetching contributors:', error)
      }
    },
    resolveId(id) {
      if (id === 'virtual:contributors') {
        return id
      }
    },
    load(id) {
      if (id === 'virtual:contributors') {
        return `export default ${JSON.stringify(contributorsData)}`
      }
    },
  }
}
