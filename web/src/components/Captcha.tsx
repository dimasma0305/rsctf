import { Box, BoxProps, useMantineColorScheme } from '@mantine/core'
import { Turnstile, TurnstileInstance } from '@marsidev/react-turnstile'
import { forwardRef, useImperativeHandle, useRef } from 'react'
import { HashPow } from '@Components/HashPow'
import { useCaptchaConfig } from '@Hooks/useConfig'
import { CaptchaProvider, type ClientCaptchaInfoModel } from '@Api'

interface CaptchaProps extends BoxProps {
  action: string
}

export interface CaptchaResult {
  valid: boolean
  token?: string | null
}

export interface CaptchaInstance {
  getToken: () => Promise<CaptchaResult>
  cleanUp?: (success?: boolean) => void
}

type RefreshCaptchaInfo = () => Promise<ClientCaptchaInfoModel | undefined>

export const resolveCaptchaInfo = async (info: ClientCaptchaInfoModel | undefined, refresh: RefreshCaptchaInfo) => {
  if (info) return info

  try {
    return await refresh()
  } catch {
    return undefined
  }
}

export const useCaptchaRef = () => {
  const captchaRef = useRef<CaptchaInstance>(null)

  const getToken = async () => {
    const res = await captchaRef.current?.getToken()
    return res ?? { valid: false }
  }

  const cleanUp = (success?: boolean) => {
    captchaRef.current?.cleanUp?.(success)
  }

  return { captchaRef, getToken, cleanUp } as const
}

export const Captcha = forwardRef<CaptchaInstance, CaptchaProps>((props, ref) => {
  const { action, ...others } = props

  const { info, error, mutate } = useCaptchaConfig()
  const { colorScheme } = useMantineColorScheme()
  const type = info?.type ?? CaptchaProvider.None

  const backendRef = useRef<CaptchaInstance>(null)

  // warp it into CaptchaInstance if necessary in the future
  const turnstileRef = useRef<TurnstileInstance>(null)

  const nonce = document.getElementById('nonce-container')?.getAttribute('data-nonce') || undefined

  useImperativeHandle(
    ref,
    () => ({
      getToken: async () => {
        // A first submit can race the initial `/api/captcha` request. Wait for
        // the authoritative policy instead of reporting an unconfigured
        // captcha as invalid. A failed refresh remains closed.
        const currentInfo = await resolveCaptchaInfo(info, mutate)
        if (!currentInfo) {
          return { valid: false }
        }

        const currentType = currentInfo.type

        if (currentType === CaptchaProvider.HashPow) {
          return backendRef.current?.getToken() ?? { valid: false }
        }

        if (currentType === CaptchaProvider.None) {
          return { valid: true }
        }

        if (currentType !== CaptchaProvider.CloudflareTurnstile || !currentInfo.siteKey) {
          return { valid: false }
        }

        // cloudflare turnstile
        const token = turnstileRef.current?.getResponse()
        return { valid: !!token, token }
      },
      cleanUp: (success?: boolean) => {
        if (type === CaptchaProvider.HashPow) {
          backendRef.current?.cleanUp?.(success)
        }
        if (type === CaptchaProvider.CloudflareTurnstile) {
          turnstileRef.current?.reset()
        }
      },
    }),
    [info, mutate, type]
  )

  if (type === CaptchaProvider.HashPow) {
    return <HashPow ref={backendRef} />
  }

  if (error || !info?.siteKey || type === CaptchaProvider.None) {
    return <Box {...others} />
  }

  return (
    <Box {...others}>
      <Turnstile
        ref={turnstileRef}
        siteKey={info.siteKey}
        options={{
          theme: colorScheme,
          action,
        }}
        scriptOptions={{
          nonce,
        }}
      />
    </Box>
  )
})
