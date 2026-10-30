import { Alert, Button, Stack, Text, TextInput } from '@mantine/core'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AccessibleModal } from '@Components/AccessibleModal'
import { Markdown } from '@Components/MarkdownRenderer'
import { RequestError } from '@Components/RequestError'
import { RetryableMutationOwner } from '@Utils/RetryableMutationOwner'
import { showErrorMsg } from '@Utils/Shared'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, { ChallengeType } from '@Api'

export function SimpleChallengeTest({
  gameId,
  challengeId,
  opened,
  onClose,
  onEnabled,
}: {
  gameId: number
  challengeId: number
  opened: boolean
  onClose: () => void
  onEnabled: () => void
}) {
  const { t } = useTranslation()
  const query = api.edit.useEditGetGameChallenge(gameId, challengeId, OnceSWRConfig)
  const [answer, setAnswer] = useState('')
  const [result, setResult] = useState<{ correct: boolean; revision: number } | null>(null)
  const [busy, setBusy] = useState(false)
  const owner = useRef(new RetryableMutationOwner())
  useEffect(() => () => owner.current.cancel(), [])
  const challenge = query.data
  const canEnable =
    !query.error &&
    challenge?.type === ChallengeType.StaticAttachment &&
    result?.correct &&
    result.revision === challenge.revision
  const enable = async () => {
    if (!canEnable || !challenge) return
    const lease = owner.current.claim(JSON.stringify({ gameId, challengeId, revision: challenge.revision }))
    if (!lease) return
    setBusy(true)
    try {
      await api.edit.editUpdateGameChallenge(
        gameId,
        challengeId,
        { isEnabled: true, expectedRevision: challenge.revision, operationId: lease.operationId },
        { signal: lease.signal }
      )
      if (!owner.current.settle(lease, true)) return
      onEnabled()
      onClose()
    } catch (error) {
      if (!owner.current.settle(lease, false)) return
      showErrorMsg(error, t)
      setBusy(false)
      void query.mutate()
    }
  }
  return (
    <AccessibleModal
      opened={opened}
      onClose={() => {
        if (!busy) onClose()
      }}
      title={challenge?.title ?? t('admin.easy.test', 'Try answer')}
      closeOnEscape={!busy}
      closeOnClickOutside={!busy}
      withCloseButton={!busy}
    >
      {query.error ? (
        <RequestError name={t('common.label.challenge', 'Challenge')} onRetry={() => query.mutate()} />
      ) : (
        challenge && (
          <Stack>
            <Markdown source={challenge.content ?? ''} />
            <Text size="sm">
              {t(
                'admin.easy.test_hint',
                'This checks the saved answer without changing scores. Use Readiness to check the full player journey.'
              )}
            </Text>
            <form
              onSubmit={(event) => {
                event.preventDefault()
                setResult({
                  correct: challenge.flags.some((flag) => flag.flag === answer.trim()),
                  revision: challenge.revision,
                })
              }}
            >
              <Stack>
                <TextInput
                  label={t('admin.easy.answer', 'Answer (flag)')}
                  value={answer}
                  onChange={(event) => {
                    setAnswer(event.currentTarget.value)
                    setResult(null)
                  }}
                  required
                  autoComplete="off"
                />
                <Button type="submit">{t('admin.easy.test', 'Try answer')}</Button>
              </Stack>
            </form>
            {result && (
              <Alert role="status" color={result.correct ? 'teal' : 'red'}>
                {result.correct
                  ? t('admin.easy.correct', 'Correct! You can enable this challenge now.')
                  : t('admin.easy.incorrect', 'That answer does not match. Try again or edit the challenge.')}
              </Alert>
            )}
            <Button disabled={!canEnable || challenge.isEnabled} loading={busy} onClick={() => void enable()}>
              {challenge.isEnabled
                ? t('admin.easy.enabled', 'Challenge enabled')
                : t('admin.easy.enable', 'Enable challenge')}
            </Button>
          </Stack>
        )
      )}
    </AccessibleModal>
  )
}
