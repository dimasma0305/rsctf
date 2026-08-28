import { Button, Center, Stack, Text, Textarea, TextInput, Title, useMantineTheme } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiCloseCircle } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { AccessibleModal, AccessibleModalProps } from '@Components/AccessibleModal'
import { createIntentStorageKey, useDurableCreateIntent } from '@Utils/DurableCreateIntent'
import { showErrorMsg } from '@Utils/Shared'
import api, { TeamUpdateModel } from '@Api'

interface TeamCreateModalProps extends AccessibleModalProps {
  disallowCreate: boolean
  mutate: () => void
  onTeamReady?: () => void
}

export const TeamCreateModal: FC<TeamCreateModalProps> = (props) => {
  const { disallowCreate, mutate, onTeamReady, ...modalProps } = props
  const [createTeam, setCreateTeam] = useState<TeamUpdateModel>({ name: '', bio: '' })
  const theme = useMantineTheme()

  const { t } = useTranslation()

  const { busy: disabled, submit } = useDurableCreateIntent({
    storageKey: createIntentStorageKey('team'),
    enabled: modalProps.opened,
    request: (payload: TeamUpdateModel, operationId, signal) =>
      api.team.teamCreateTeam({ ...payload, operationId }, { signal }),
    onSuccess: (res) => {
      showNotification({
        color: 'teal',
        title: t('team.notification.create.success.title'),
        message: t('team.notification.create.success.message', { team: res.data.name }),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      setCreateTeam({ name: '', bio: '' })
      onTeamReady?.()
      mutate()
      modalProps.onClose()
    },
    onError: (error) => showErrorMsg(error, t),
  })

  const onCreateTeam = async () => {
    if (disabled || !(createTeam.name?.trim().length ?? 0)) return
    await submit(createTeam)
  }

  const handleClose = () => {
    if (disabled) return
    setCreateTeam({ name: '', bio: '' })
    modalProps.onClose()
  }

  return (
    <AccessibleModal {...modalProps} onClose={handleClose} closeOnClickOutside={!disabled} closeOnEscape={!disabled}>
      {disallowCreate ? (
        <Stack gap="lg" p={40} ta="center">
          <Center>
            <Icon color={theme.colors.red[7]} path={mdiCloseCircle} size={4} />
          </Center>
          <Title order={3}>{t('team.content.disallow_create.title')}</Title>
          <Text>
            <Trans i18nKey="team.content.disallow_create.content" />
          </Text>
        </Stack>
      ) : (
        <Stack
          component="form"
          data-guide="team-create-workflow"
          data-guide-stage={(createTeam.name?.trim().length ?? 0) > 0 ? 'submit' : 'input'}
          data-guide-interaction-scope
          onSubmit={(event) => {
            event.preventDefault()
            void onCreateTeam()
          }}
        >
          <Text>{t('team.content.create')}</Text>
          <TextInput
            data-guide="team-create-name"
            label={t('team.label.name')}
            description={t(
              'team.content.create_name_hint',
              'Type a team name. Create Team becomes available when the field is not empty.'
            )}
            type="text"
            placeholder={t('team.placeholder.name', 'Type your team name')}
            w="100%"
            disabled={disabled}
            maxLength={128}
            value={createTeam?.name ?? ''}
            onChange={(event) => setCreateTeam({ ...createTeam, name: event.currentTarget.value })}
          />
          <Textarea
            label={t('team.label.bio')}
            placeholder={createTeam?.bio ?? t('team.placeholder.bio')}
            value={createTeam?.bio ?? ''}
            w="100%"
            autosize
            minRows={2}
            maxRows={4}
            disabled={disabled}
            maxLength={4096}
            onChange={(event) => setCreateTeam({ ...createTeam, bio: event.currentTarget.value })}
          />
          <Button
            type="submit"
            fullWidth
            variant="outline"
            disabled={disabled || (createTeam.name?.trim().length ?? 0) === 0}
            data-guide="team-create-submit"
          >
            {t('team.button.create')}
          </Button>
        </Stack>
      )}
    </AccessibleModal>
  )
}
