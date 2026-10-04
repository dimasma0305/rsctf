import { BoxProps, Button, Group, InputBase, Text, InputBaseProps } from '@mantine/core'
import { forwardRef, useState, useEffect, useImperativeHandle, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { CaptchaInstance } from '@Components/Captcha'
import { PowWorker } from '@Components/icon/PowWorker'
import workerScript from '@Utils/PowWorker'
import { showErrorMsg } from '@Utils/Shared'
import api, { HashPowChallenge } from '@Api'
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

export const usePowChallenge = () => {
  const { t } = useTranslation()
  const [chall, setChall] = useState<HashPowChallenge | undefined>()
  const [result, setResult] = useState<PowResult | null>(null)
  const [error, setError] = useState(false)
  const [pending, setPending] = useState(false)
  const workerRef = useRef<Worker | null>(null)
  const fetchRef = useRef<AbortController | null>(null)
  const generationRef = useRef(0)

  const stopCurrentWork = () => {
    generationRef.current += 1
    fetchRef.current?.abort()
    fetchRef.current = null
    workerRef.current?.terminate()
    workerRef.current = null
    setPending(false)
  }

  const solve = (challenge: HashPowChallenge, generation: number) => {
    if (!challenge.challenge || !challenge.difficulty) {
      setError(true)
      return
    }
    const worker = new Worker(workerScript)
    workerRef.current = worker
    setPending(true)
    worker.onmessage = (event: MessageEvent<PowResult>) => {
      if (generationRef.current !== generation || workerRef.current !== worker) return
      worker.terminate()
      workerRef.current = null
      setPending(false)
      if (event.data.nonce) {
        setResult(event.data)
        setError(false)
      } else {
        setError(true)
      }
    }
    worker.onerror = () => {
      if (generationRef.current !== generation || workerRef.current !== worker) return
      worker.terminate()
      workerRef.current = null
      setPending(false)
      setError(true)
    }
    worker.postMessage({ chall: challenge.challenge, diff: challenge.difficulty } as PowRequest)
  }

  const fetchPowChallenge = async () => {
    stopCurrentWork()
    const generation = generationRef.current
    const controller = new AbortController()
    fetchRef.current = controller
    setChall(undefined)
    setResult(null)
    setError(false)
    setPending(true)
    try {
      const response = await api.info.infoPowChallenge({ signal: controller.signal })
      if (generationRef.current !== generation || controller.signal.aborted) return null
      const challenge = response.data
      if (
        !challenge?.id ||
        !challenge.challenge ||
        !challenge.difficulty ||
        !challenge.expiresAt ||
        challenge.expiresAt <= Date.now() + 1_000
      ) throw new Error('Invalid or expired proof-of-work challenge')
      fetchRef.current = null
      setChall(challenge)
      setPending(false)
      solve(challenge, generation)
      return challenge
    } catch (e) {
      if (controller.signal.aborted || generationRef.current !== generation) return null
      fetchRef.current = null
      setPending(false)
      setError(true)
      showErrorMsg(e, t)
      return null
    }
  }

  useEffect(() => {
    void fetchPowChallenge()
    return stopCurrentWork
  }, [])

  useEffect(() => {
    if (!chall?.expiresAt) return
    const refreshIn = Math.max(0, chall.expiresAt - Date.now() - 5_000)
    const timer = setTimeout(() => void fetchPowChallenge(), refreshIn)
    return () => clearTimeout(timer)
  }, [chall?.id, chall?.expiresAt])

  return { chall, error, mutate: fetchPowChallenge, pending, result: pending ? null : result }
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
      setRand(array.reduce((acc, val) => acc + val.toString(16).padStart(8, '0'), ''))
    }, 76)
    return () => clearInterval(interval)
  }, [nonce])

  const done = !!nonce || undefined

  return (
    <Group
      {...rest}
      ref={ref}
      display="flex"
      wrap="nowrap"
      gap={0}
      justify="space-between"
      className={classes.container}
    >
      <PowWorker done={done} />
      <Text data-done={done} className={classes.text}>
        {nonce || rand}
      </Text>
    </Group>
  )
})

export const HashPow = forwardRef<CaptchaInstance, InputBaseProps>((props, ref) => {
  const { t } = useTranslation()
  const { chall, result, error, mutate, pending } = usePowChallenge()

  useImperativeHandle(
    ref,
    () => ({
      getToken: async () => {
        if (chall?.id && chall.expiresAt && chall.expiresAt > Date.now() && result?.nonce) {
          return { valid: true, token: `${chall?.id}:${result.nonce}` }
        } else {
          return { valid: false }
        }
      },
      cleanUp: (success?: boolean) => {
        if (!success) {
          // refresh challenge on failure
          mutate()
        }
      },
    }),
    [chall, result]
  )

  return (
    <>
      <InputBase
        {...props}
        w="100%"
        required
        variant="unstyled"
        label={t('account.label.captcha')}
        description={
          !error && result
            ? `${result.time / 1000}s @ ${result.rate.toFixed(2)} kH/s`
            : t('account.placeholder.computing')
        }
        component={PowBox}
        nonce={result?.nonce}
      />
      {error && !pending && (
        <Button mt="xs" size="xs" variant="light" onClick={() => void mutate()}>
          {t('account.button.retry_captcha', 'Retry challenge')}
        </Button>
      )}
    </>
  )
})
