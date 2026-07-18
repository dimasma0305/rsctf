import { useDocumentTitle } from '@mantine/hooks'
import { useConfig } from '@Hooks/useConfig'

export const usePageTitle = (title?: string) => {
  const { config, error } = useConfig()

  const platform = error ? 'RS::CTF' : `${config?.title?.trim() || 'RS'}::CTF`

  useDocumentTitle(typeof title === 'string' && title.trim().length > 0 ? `${title} - ${platform}` : platform)
}
