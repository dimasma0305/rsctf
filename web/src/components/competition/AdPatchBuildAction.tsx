import { Button, Group, Text } from '@mantine/core'
import { useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import useSWR from 'swr'
import api from '@Api'
import { CompletionPollSWRConfig, useCompletionPolling } from '@Hooks/useCompletionPolling'
import { assertJsonResponse, createChallengePollOwner } from '@Utils/ChallengePolling'
import { showErrorMsg } from '@Utils/Shared'

export function AdPatchBuildAction({ gameId, challengeId, serviceId, revision, commitId }: {
  gameId: number; challengeId: number; serviceId: number; revision: number; commitId: string
}) {
  const { t } = useTranslation()
  const pending = useRef<AbortController | null>(null)
  const reader = useMemo(createChallengePollOwner, [])
  const [sending, setSending] = useState(false)
  useEffect(() => () => { pending.current?.abort(); reader.cancel() }, [reader])
  const key = `/api/game/${gameId}/ad/challenges/${challengeId}/patch-revisions/${revision}/build?service=${serviceId}&commit=${commitId}`
  const { data, error, mutate, isValidating } = useSWR(key, async () => {
    const controller = reader.begin()
    try {
      return assertJsonResponse(await api.game.adPatchBuild(
        gameId, challengeId, revision, serviceId, commitId, 'GET', undefined, { signal: controller.signal }
      ))
    } finally {
      reader.finish(controller)
    }
  }, { ...CompletionPollSWRConfig, revalidateOnMount: true })
  useCompletionPolling({
    key, phase: 'build', enabled: true, data, error, isValidating, mutate,
    successDelay: build => build.status === 'Queued' || build.status === 'Building' ? 15_000 : null,
  })
  const submit = async () => {
    if (!data?.available || pending.current) return
    const controller = new AbortController()
    pending.current = controller
    setSending(true)
    try {
      const build = assertJsonResponse(await api.game.adPatchBuild(
        gameId, challengeId, revision, serviceId, commitId, 'POST', data.attempt ?? undefined, { signal: controller.signal }
      ))
      if (!controller.signal.aborted) await mutate(build, { revalidate: false })
    } catch (cause) {
      if (!controller.signal.aborted) {
        showErrorMsg(cause, t)
        void mutate().catch(() => {})
      }
    } finally {
      pending.current = null
      if (!controller.signal.aborted) setSending(false)
    }
  }
  if (error) return <Button variant="subtle" size="xs" onClick={() => void mutate().catch(() => {})}>{t('game.ad.patch.reload_build', 'Reload build status')}</Button>
  if (!data || (!data.available && !data.status)) return null
  const message = {
    Queued: t('game.ad.patch.queued', 'Build queued'),
    Building: t('game.ad.patch.building', 'Building in an isolated VM…'),
    Ready: data.staged
      ? t('game.ad.patch.staged', 'Image verified in the registry. Your service is still running its current version.')
      : t('game.ad.patch.ready', 'Candidate ready. Your service is still running its current version.'),
    Failed: t('game.ad.patch.failed', 'Build failed. Your service keeps running.'),
    Replaced: t('game.ad.patch.replaced', 'Another revision is the current candidate.'),
  }
  const canBuild = !data.status || data.status === 'Failed' || data.status === 'Replaced'
  const waiting = !data.available && (data.status === 'Queued' || data.status === 'Building')
  return <Group gap="xs">
    {data.status && <Text size="sm" role="status">
      {waiting ? t('game.ad.patch.waiting_worker', 'Waiting for the challenge builder to reconnect.') : message[data.status]}
    </Text>}
    {data.status === 'Ready' && !data.staged && <Button size="xs" variant="subtle" loading={isValidating}
      onClick={() => void mutate().catch(() => {})}>{t('game.ad.patch.reload_build', 'Reload build status')}</Button>}
    {data.available && canBuild && <Button variant="light" size="xs" loading={sending} onClick={() => void submit()}>
      {t('game.ad.patch.build_revision', 'Build revision {{number}}', { number: revision })}
    </Button>}
  </Group>
}
