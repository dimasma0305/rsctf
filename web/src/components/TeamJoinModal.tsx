import { Button, Stack, Text, TextInput } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { AccessibleModal, AccessibleModalProps } from '@Components/AccessibleModal'
import { encryptApiData } from '@Utils/Crypto'
import { showErrorMsg } from '@Utils/Shared'
import { isValidTeamInviteCode } from '@Utils/TeamInvite'
import { settleTeamJoinAttempt } from '@Utils/TeamJoinFlow'
import api from '@Api'

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
  ...modalProps
}) => {
  const [joining, setJoining] = useState(false)
  const { t } = useTranslation()
  const validCode = isValidTeamInviteCode(code)

  const onJoinTeam = async () => {
    if (!validCode) {
      showNotification({
        color: 'red',
        title: t('common.error.encountered'),
        message: t('team.notification.join.wrong_invite_code'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    setJoining(true)
    try {
      await settleTeamJoinAttempt({
        accept: async () => {
          const identity = enableBrowserFingerprint
            ? await (async () => {
                const challengeResponse = await api.account.accountFingerprintChallenge()
                const challenge = challengeResponse.data.data
                if (!challenge?.nonce || !challenge.requiredSignals) {
                  throw new Error('Invalid fingerprint challenge')
                }
                const { getFingerprintPayload } = await import('@Utils/BrowserFingerprint')
                const payload = await getFingerprintPayload({
                  nonce: challenge.nonce,
                  requiredSignals: challenge.requiredSignals,
                })
                return {
                  code,
                  fingerprint: await encryptApiData(t, payload.fingerprint, apiPublicKey),
                  fingerprintProof: await encryptApiData(t, payload.proof, apiPublicKey),
                }
              })()
            : { code }
          await api.team.teamAccept(identity)
        },
        onAccepted: () => {
          showNotification({
            color: 'teal',
            title: t('team.notification.join.success'),
            message: t('team.notification.updated'),
            icon: <Icon path={mdiCheck} size={1} />,
          })
          onTeamReady?.()
          mutate()
          onCodeChange('')
          modalProps.onClose()
        },
        onRejected: (error) => showErrorMsg(error, t),
      })
    } finally {
      setJoining(false)
    }
  }

  return (
    <AccessibleModal {...modalProps}>
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
        <TextInput
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
          onChange={(event) => onCodeChange(event.currentTarget.value)}
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
