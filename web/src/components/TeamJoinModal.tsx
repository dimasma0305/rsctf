import { Alert, Button, Stack, Text, TextInput } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AccessibleModal, AccessibleModalProps } from '@Components/AccessibleModal'
import { submitTeamEnrollment } from '@Utils/EnrollmentFlow'
import { showErrorMsg, tryGetErrorMsg } from '@Utils/Shared'
import { isValidTeamInviteCode } from '@Utils/TeamInvite'
import { settleTeamJoinAttempt } from '@Utils/TeamJoinFlow'

interface TeamJoinModalProps extends AccessibleModalProps {
  code: string
  onCodeChange: (code: string) => void
  mutate: () => void
  onTeamReady?: () => void
  enableBrowserFingerprint?: boolean
  apiPublicKey?: string | null
}

export const TeamJoinModal: FC<TeamJoinModalProps> = ({
  code,
  onCodeChange,
  mutate,
  onTeamReady,
  enableBrowserFingerprint,
  apiPublicKey,
  opened,
  onClose,
  ...modalProps
}) => {
  const [joining, setJoining] = useState(false)
  const [joinError, setJoinError] = useState<string | null>(null)
  const attemptGeneration = useRef(0)
  const attemptInFlight = useRef(false)
  const attemptAbort = useRef<AbortController | null>(null)
  const codeInputRef = useRef<HTMLInputElement>(null)
  const { t } = useTranslation()
  const validCode = isValidTeamInviteCode(code)

  const invalidateAttempt = useCallback(() => {
    attemptGeneration.current += 1
    attemptAbort.current?.abort()
    attemptAbort.current = null
    attemptInFlight.current = false
  }, [])

  useEffect(() => () => invalidateAttempt(), [invalidateAttempt])

  const resetAttempt = useCallback(() => {
    invalidateAttempt()
    setJoining(false)
    setJoinError(null)
  }, [invalidateAttempt])

  const previousOpened = useRef(opened)
  useEffect(() => {
    if (previousOpened.current === opened) return
    previousOpened.current = opened
    resetAttempt()
  }, [opened, resetAttempt])

  useEffect(() => {
    if (!joinError) return
    const timer = setTimeout(() => codeInputRef.current?.focus(), 0)
    return () => clearTimeout(timer)
  }, [joinError])

  const closeAndReset = () => {
    resetAttempt()
    onClose()
  }

  const onJoinTeam = async () => {
    if (attemptInFlight.current) return

    if (!validCode) {
      const message = t('team.notification.join.wrong_invite_code')
      setJoinError(message)
      showNotification({
        color: 'red',
        title: t('common.error.encountered'),
        message,
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    const generation = ++attemptGeneration.current
    const controller = new AbortController()
    attemptAbort.current?.abort()
    attemptAbort.current = controller
    attemptInFlight.current = true
    setJoinError(null)
    setJoining(true)
    try {
      await settleTeamJoinAttempt({
        accept: () =>
          submitTeamEnrollment({ code, enableBrowserFingerprint, apiPublicKey, signal: controller.signal, t }),
        onAccepted: () => {
          if (generation !== attemptGeneration.current) return
          showNotification({
            color: 'teal',
            title: t('team.notification.join.success'),
            message: t('team.notification.updated'),
            icon: <Icon path={mdiCheck} size={1} />,
          })
          onTeamReady?.()
          mutate()
          onCodeChange('')
          closeAndReset()
        },
        onRejected: (error) => {
          if (generation !== attemptGeneration.current) return
          // Publish the retryable state only after releasing the synchronous
          // owner, so a player can act on the visible error immediately.
          attemptAbort.current = null
          attemptInFlight.current = false
          setJoining(false)
          setJoinError(tryGetErrorMsg(error, t))
          showErrorMsg(error, t)
        },
      })
    } finally {
      if (generation === attemptGeneration.current) {
        attemptAbort.current = null
        attemptInFlight.current = false
        setJoining(false)
      }
    }
  }

  return (
    <AccessibleModal {...modalProps} opened={opened} onClose={closeAndReset}>
      <Stack
        component="form"
        data-guide="team-join-workflow"
        data-guide-stage={validCode ? 'submit' : 'input'}
        data-guide-interaction-scope
        onSubmit={(event) => {
          event.preventDefault()
          void onJoinTeam()
        }}
      >
        <Text size="sm">{t('team.content.join')}</Text>
        {joinError && (
          <Alert color="red" role="alert" title={t('common.error.encountered')}>
            {joinError}
          </Alert>
        )}
        <TextInput
          ref={codeInputRef}
          data-guide="team-join-code"
          label={t('team.label.invite_code')}
          description={t(
            'team.content.join_code_hint',
            'Paste the complete invite code from your teammate, then select Join.'
          )}
          type="text"
          placeholder="team:0:01234567890123456789012345678901"
          w="100%"
          value={code}
          error={joinError ? t('common.error.check_input') : undefined}
          disabled={joining}
          onChange={(event) => {
            setJoinError(null)
            onCodeChange(event.currentTarget.value)
          }}
        />
        <Button
          type="submit"
          fullWidth
          variant="outline"
          loading={joining}
          disabled={joining || !validCode}
          data-guide="team-join-submit"
        >
          {t('team.button.join')}
        </Button>
      </Stack>
    </AccessibleModal>
  )
}
