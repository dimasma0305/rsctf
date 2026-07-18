import dayjs from 'dayjs'
import { useEffect, useRef } from 'react'
import { SWRConfiguration } from 'swr'
import api, { ClientConfig, ContainerPortMappingType } from '@Api'

export const RSCTF_REPOSITORY = 'https://github.com/dimasma0305/rsctf'
export const RSCTF_DOCUMENTATION = `${RSCTF_REPOSITORY}/tree/main/docs`

export const OnceSWRConfig: SWRConfiguration = {
  refreshInterval: 0,
  revalidateOnFocus: false,
}

const fallbackConfig: ClientConfig = {
  title: 'RS',
  slogan: 'Capture. Compete. Conquer.',
  portMapping: ContainerPortMappingType.Default,
  footerInfo: null,
  customTheme: null,
  defaultLifetime: 120,
  extensionDuration: 120,
  renewalWindow: 10,
  enableBrowserFingerprint: false,
}

export const useConfig = () => {
  const query = api.info.useInfoGetClientConfig({
    refreshInterval: 0,
    revalidateOnFocus: false,
    revalidateOnReconnect: false,
    refreshWhenHidden: false,
    shouldRetryOnError: false,
    refreshWhenOffline: false,
  })

  return { config: query.data ?? fallbackConfig, error: query.error, mutate: query.mutate }
}

export const useCaptchaConfig = () => {
  const query = api.info.useInfoGetClientCaptchaInfo({
    refreshInterval: 0,
    revalidateOnFocus: false,
    revalidateOnReconnect: false,
    refreshWhenHidden: false,
    shouldRetryOnError: false,
    refreshWhenOffline: false,
  })

  return { info: query.data, error: query.error, mutate: query.mutate }
}

const repoMeta = {
  sha: import.meta.env.VITE_APP_GIT_SHA ?? 'unknown',
  rawTag: import.meta.env.VITE_APP_GIT_NAME ?? 'main',
  timestamp: import.meta.env.VITE_APP_BUILD_TIMESTAMP ?? '',
  repo: RSCTF_REPOSITORY,
}

export const ValidatedRepoMeta = () => {
  const buildTime = dayjs(repoMeta.timestamp)
  const tag = repoMeta.rawTag.replace(/-.*$/, '')
  const valid = /^[0-9a-f]{40}$/i.test(repoMeta.sha) && buildTime.isValid()

  return { ...repoMeta, tag, buildTime, valid }
}

export const useBanner = () => {
  const shown = useRef(false)

  useEffect(() => {
    if (shown.current) return
    shown.current = true

    const { sha, tag, buildTime, valid } = ValidatedRepoMeta()
    console.info(
      [
        'RS::CTF web client',
        `Source: ${RSCTF_REPOSITORY}`,
        `Revision: ${valid ? `${tag} (${sha})` : 'local/source build'}`,
        `Built: ${buildTime.isValid() ? buildTime.format('YYYY-MM-DDTHH:mm:ssZ') : 'metadata unavailable'}`,
        'Legal notices: /legal/NOTICE',
      ].join('\n')
    )
  }, [])
}
