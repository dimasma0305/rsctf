import {
  ActionIcon,
  Avatar,
  Badge,
  Card,
  Group,
  CardProps,
  Stack,
  Text,
  useMantineColorScheme,
  useMantineTheme,
} from '@mantine/core'
import { mdiCheckCircle, mdiCircleOutline, mdiDownload } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { ScrollingText } from '@Components/ScrollingText'
import { useLanguage } from '@Utils/I18n'
import { WriteupInfo } from '@Api'
import misc from '@Styles/Misc.module.css'

interface TeamWriteupCardProps extends CardProps {
  writeup: WriteupInfo
  selected?: boolean
  onClick: () => void
  divisionName?: string
}

export const TeamWriteupCard: FC<TeamWriteupCardProps> = ({ writeup, selected, divisionName, onClick, ...props }) => {
  const { colorScheme } = useMantineColorScheme()
  const { locale } = useLanguage()
  const { t } = useTranslation()
  const theme = useMantineTheme()
  const borderColor = selected ? theme.colors[theme.primaryColor][colorScheme === 'dark' ? 8 : 6] : 'transparent'

  return (
    <Card
      {...props}
      component="article"
      onClick={onClick}
      p="sm"
      shadow="sm"
      classNames={{ root: misc.hoverCard }}
      bd={`2px solid ${borderColor}`}
      data-no-move
    >
      <Group wrap="nowrap" gap={3} justify="space-between">
        <Group gap="sm" wrap="nowrap" justify="space-between" maw="calc(100% - 2rem)">
          <Avatar
            imageProps={{ loading: 'lazy' }}
            alt={t('admin.content.team_avatar', '{{team}} avatar', { team: writeup.team?.name ?? '' })}
            src={writeup.team?.avatar}
            size="md"
          >
            {writeup.team?.name?.slice(0, 1)}
          </Avatar>
          <Stack gap={0} justify="space-between" maw="calc(100% - 3rem)">
            <Group gap="xs">
              <Text size="0.8rem" lineClamp={1} c="dimmed">
                #{writeup.team?.id}
              </Text>
              {divisionName && (
                <Badge size="xs" variant="light">
                  {divisionName}
                </Badge>
              )}
            </Group>
            <ScrollingText size="md" fw={600} text={writeup.team?.name ?? ''} />
            <Text size="xs" lineClamp={1} c="dimmed">
              {dayjs(writeup.uploadTimeUtc).locale(locale).format('SLL LT')}
            </Text>
          </Stack>
        </Group>
        <Group gap={4} wrap="nowrap">
          <ActionIcon
            color={selected ? 'brand' : 'gray'}
            aria-pressed={selected}
            aria-label={t('admin.button.writeups.select', 'Select {{team}} writeup', {
              team: writeup.team?.name ?? '',
            })}
            onClick={(event) => {
              event.stopPropagation()
              onClick()
            }}
          >
            <Icon path={selected ? mdiCheckCircle : mdiCircleOutline} size={1} />
          </ActionIcon>
          <ActionIcon
            component={Link}
            target="_blank"
            rel="noreferrer"
            to={writeup.url ?? '#'}
            aria-label={t('admin.button.writeups.download', 'Download writeup')}
            onClick={(event) => event.stopPropagation()}
          >
            <Icon path={mdiDownload} size={1} />
          </ActionIcon>
        </Group>
      </Group>
    </Card>
  )
}
