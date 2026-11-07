import { useDocumentTitle } from '@mantine/hooks'
import { useEffect, useLayoutEffect } from 'react'
import { useLocation } from 'react-router'
import { isIndexablePage, updatePageMetadata } from '@Utils/PageMetadata'
import { useConfig } from '@Hooks/useConfig'

interface PageSeoOptions {
  description?: string | null
  imageUrl?: string | null
  indexable?: boolean
}

// Clear metadata before a new route paints, including routes still loading.
export const useResetPageMetadata = () => {
  const { pathname } = useLocation()
  useLayoutEffect(() => {
    updatePageMetadata(
      {
        title: 'RS::CTF',
        path: pathname,
        indexable: false,
      },
      document,
      window.location.origin
    )
  }, [pathname])
}

export const usePageTitle = (title?: string, options: PageSeoOptions = {}) => {
  const { config, error } = useConfig()
  const { pathname } = useLocation()

  const platform = error ? 'RS::CTF' : `${config?.title?.trim() || 'RS'}::CTF`

  const pageTitle = typeof title === 'string' && title.trim().length > 0 ? `${title} - ${platform}` : platform
  useDocumentTitle(pageTitle)
  const indexable = isIndexablePage(pathname, options.indexable)
  const description = indexable ? options.description || config.description : config.description
  const imageUrl = indexable ? options.imageUrl || config.logoUrl : config.logoUrl
  useEffect(() => {
    updatePageMetadata(
      {
        title: indexable ? pageTitle : platform,
        description,
        imageUrl,
        publicUrl: config.publicUrl,
        path: pathname,
        indexable,
      },
      document,
      window.location.origin
    )
  }, [pageTitle, platform, description, imageUrl, config.publicUrl, pathname, indexable])
}
