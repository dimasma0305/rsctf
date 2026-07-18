import { ActionIcon, Card, Group, Input, SimpleGrid, Stack, Text } from '@mantine/core'
import { useClipboard } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import { mdiCheck, mdiDeleteOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { useDisplayInputStyles } from '@Utils/ThemeOverride'
import { Attachment, FlagInfoModel } from '@Api'

interface FlagCardProps {
  flag: FlagInfoModel
  onDelete: () => void
  unifiedAttachment?: Attachment | null
}

const FlagCard: FC<FlagCardProps> = ({ flag, onDelete, unifiedAttachment }) => {
  const clipboard = useClipboard()
  const attachment = unifiedAttachment ?? flag.attachment
  const shortURL = attachment?.url?.split('/').slice(-2)[0].slice(0, 8)
  const { classes } = useDisplayInputStyles({ fw: 'bold', ff: 'monospace', cs: 'pointer' })
  const { t } = useTranslation()

  const copyFlag = () => {
    clipboard.copy(flag.flag)
    showNotification({
      message: t('admin.notification.games.challenges.flag.copied'),
      color: 'teal',
      icon: <Icon path={mdiCheck} size={1} />,
    })
  }

  return (
    <Card p="sm">
      <Group wrap="nowrap" justify="space-between" gap={3}>
        <Stack align="flex-start" gap={0} w="100%" style={{ minWidth: 0 }}>
          <Input
            variant="unstyled"
            value={flag.flag}
            aria-label={t('admin.tooltip.games.challenges.flag.copy', 'Copy flag')}
            w="100%"
            size="md"
            readOnly
            onClick={copyFlag}
            onKeyDown={(event) => {
              if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault()
                copyFlag()
              }
            }}
            classNames={classes}
          />
          <Text c="dimmed" size="sm" ff="monospace">
            {attachment?.type} {shortURL}
          </Text>
        </Stack>
        <ActionIcon onClick={onDelete} color="red" aria-label={t('admin.button.challenges.flag.delete', 'Delete flag')}>
          <Icon path={mdiDeleteOutline} size={1} />
        </ActionIcon>
      </Group>
    </Card>
  )
}

interface FlagEditPanelProps {
  flags?: FlagInfoModel[]
  onDelete: (flag: FlagInfoModel) => void
  unifiedAttachment?: Attachment | null
}

export const FlagEditPanel: FC<FlagEditPanelProps> = ({ flags, onDelete, unifiedAttachment }) => {
  return (
    <Stack>
      <SimpleGrid spacing="sm" cols={{ base: 1, sm: 2, w18: 3, w24: 4, w30: 5, w36: 6, w42: 7, w48: 8 }}>
        {flags &&
          flags.map((flag, i) => (
            <FlagCard key={i} flag={flag} onDelete={() => onDelete(flag)} unifiedAttachment={unifiedAttachment} />
          ))}
      </SimpleGrid>
    </Stack>
  )
}
