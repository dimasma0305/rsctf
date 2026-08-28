import { Button, Group, InputBase, Text, type BoxProps, type InputBaseProps } from '@mantine/core'
import { forwardRef, useCallback, useEffect, useImperativeHandle, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { type CaptchaInstance } from '@Components/Captcha'
import { PowWorker } from '@Components/icon/PowWorker'
import workerScript from '@Utils/PowWorker'
import api, { type HashPowChallenge } from '@Api'
import classes from '@Styles/HashPow.module.css'

export interface PowRequest {
  chall: string
  diff: number
}

export interface PowResult {
  nonce: string | null
  time: number
  rate: number
}

const EXPIRY_SAFETY_MS = 5_000

type ActiveHashPowChallenge = HashPowChallenge & {
  id: string
  challenge: string
  difficulty: number
  expiresAt: number
}

interface PowState {
  challenge: ActiveHashPowChallenge | null
  result: PowResult | null
  status: 'loading' | 'solving' | 'ready' | 'error'
}

/**
 * Own exactly one request and one worker for the currently mounted captcha.
 * Challenges are deliberately memory-only: persisting a bearer proof allows a
 * later tab or user session to replay private authentication material.
 */
export const usePowChallenge = () => {
  const [state, setState] = useState<PowState>({ challenge: null, result: null, status: 'loading' })
  const requestRef = useRef<AbortController | null>(null)
  const workerRef = useRef<Worker | null>(null)
  const expiryRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const generationRef = useRef(0)

  const cancelCurrent = useCallback(() => {
    requestRef.current?.abort()
    requestRef.current = null
    workerRef.current?.terminate()
    workerRef.current = null
    if (expiryRef.current !== null) clearTimeout(expiryRef.current)
    expiryRef.current = null
  }, [])

  const refresh = useCallback(async () => {
    cancelCurrent()
    const generation = ++generationRef.current
    const controller = new AbortController()
    requestRef.current = controller
    setState({ challenge: null, result: null, status: 'loading' })

    try {
      const response = await api.info.infoPowChallenge({ signal: controller.signal })
      if (controller.signal.aborted || generation !== generationRef.current) return
      requestRef.current = null
      const candidate = response.data
      if (
        !candidate ||
        typeof candidate.id !== 'string' ||
        typeof candidate.challenge !== 'string' ||
        typeof candidate.difficulty !== 'number' ||
        typeof candidate.expiresAt !== 'number' ||
        candidate.expiresAt <= Date.now() + EXPIRY_SAFETY_MS
      ) {
        setState({ challenge: null, result: null, status: 'error' })
        return
      }
      const challenge: ActiveHashPowChallenge = {
        ...candidate,
        id: candidate.id,
        challenge: candidate.challenge,
        difficulty: candidate.difficulty,
        expiresAt: candidate.expiresAt,
      }

      const worker = new Worker(workerScript)
      workerRef.current = worker
      setState({ challenge, result: null, status: 'solving' })
      expiryRef.current = setTimeout(
        () => void refresh(),
        Math.max(0, challenge.expiresAt - Date.now() - EXPIRY_SAFETY_MS)
      )
      worker.onmessage = (event: MessageEvent<PowResult>) => {
        if (generation !== generationRef.current || worker !== workerRef.current) return
        worker.terminate()
        workerRef.current = null
        const result = event.data
        setState({
          challenge,
          result: result.nonce ? result : null,
          status: result.nonce ? 'ready' : 'error',
        })
      }
      worker.onerror = () => {
        if (generation !== generationRef.current || worker !== workerRef.current) return
        worker.terminate()
        workerRef.current = null
        setState({ challenge, result: null, status: 'error' })
      }
      worker.postMessage({ chall: challenge.challenge, diff: challenge.difficulty } satisfies PowRequest)
    } catch {
      if (!controller.signal.aborted && generation === generationRef.current) {
        requestRef.current = null
        setState({ challenge: null, result: null, status: 'error' })
      }
    }
  }, [cancelCurrent])

  useEffect(() => {
    void refresh()
    return () => {
      generationRef.current += 1
      cancelCurrent()
    }
  }, [cancelCurrent, refresh])

  return { ...state, refresh }
}

interface PowBoxProps extends BoxProps {
  nonce?: string | null
}

const PowBox = forwardRef<HTMLDivElement, PowBoxProps>((props, ref) => {
  const { nonce, ...rest } = props
  const [rand, setRand] = useState<string>('0e5cd7b6c765abbf')

  useEffect(() => {
    if (nonce) return

    const array = new Uint32Array(2)
    const interval = setInterval(() => {
      crypto.getRandomValues(array)
      setRand(array.reduce((acc, value) => acc + value.toString(16).padStart(8, '0'), ''))
    }, 76)
    return () => clearInterval(interval)
  }, [nonce])

  const done = !!nonce || undefined
  return (
    <Group {...rest} ref={ref} wrap="nowrap" gap={0} justify="space-between" className={classes.container}>
      <PowWorker done={done} />
      <Text data-done={done} className={classes.text}>
        {nonce || rand}
      </Text>
    </Group>
  )
})

export const HashPow = forwardRef<CaptchaInstance, InputBaseProps>((props, ref) => {
  const { t } = useTranslation()
  const { challenge, result, status, refresh } = usePowChallenge()

  useImperativeHandle(
    ref,
    () => ({
      getToken: async () => {
        if (challenge && result?.nonce && challenge.expiresAt > Date.now() + EXPIRY_SAFETY_MS) {
          return { valid: true, token: `${challenge.id}:${result.nonce}` }
        }
        return { valid: false }
      },
      cleanUp: (success?: boolean) => {
        if (!success) void refresh()
      },
    }),
    [challenge, refresh, result]
  )

  const description =
    status === 'ready' && result
      ? `${result.time / 1000}s @ ${result.rate.toFixed(2)} kH/s`
      : status === 'error'
        ? t('account.captcha.failed', 'Proof of work could not be prepared.')
        : t('account.placeholder.computing')

  return (
    <Group align="flex-end" wrap="nowrap" gap="xs">
      <InputBase
        {...props}
        w="100%"
        required
        variant="unstyled"
        label={t('account.label.captcha')}
        description={description}
        component={PowBox}
        nonce={result?.nonce}
        aria-live="polite"
      />
      {status === 'error' && (
        <Button mb={4} size="xs" variant="light" onClick={() => void refresh()}>
          {t('common.button.retry', 'Retry')}
        </Button>
      )}
    </Group>
  )
})
