import { Group, Text } from '@mantine/core'
import dayjs, { Dayjs } from 'dayjs'
import duration from 'dayjs/plugin/duration'
import localizedFormat from 'dayjs/plugin/localizedFormat'
import { FC, useEffect, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { useServerNow } from '@Utils/ServerClock'

dayjs.extend(duration)
dayjs.extend(localizedFormat)

export interface ChallengeDeadlineNoticeProps {
  deadline: Dayjs
  locale: string
  onExpiredChange: (expired: boolean) => void
}

export const ChallengeDeadlineNotice: FC<ChallengeDeadlineNoticeProps> = ({ deadline, locale, onExpiredChange }) => {
  const { t } = useTranslation()
  const now = useServerNow()
  const expired = now.isAfter(deadline)
  const formattedDeadline = useMemo(() => deadline.locale(locale).format('L LTS'), [deadline, locale])

  useEffect(() => {
    onExpiredChange(expired)
  }, [expired, onExpiredChange])

  if (expired) {
    return null
  }

  const diff = deadline.diff(now)
  const remaining = dayjs.duration(diff)
  const countdownText = `${Math.floor(remaining.asHours())}:${remaining.format('mm:ss')}`

  return (
    <Group gap="xs" justify="space-between" wrap="nowrap">
      <Text fw="bold" size="sm">
        {t('challenge.content.deadline.remaining')}&nbsp;
        <Text span ff="monospace" fw="bold" size="sm" c="brand">
          {countdownText}
        </Text>
      </Text>
      <Text fw="bold" size="xs" c="dimmed">
        {t('challenge.content.deadline.label')}&nbsp;
        <Text span ff="monospace" c="dimmed" fw="bold" size="xs">
          {formattedDeadline}
        </Text>
      </Text>
    </Group>
  )
}
