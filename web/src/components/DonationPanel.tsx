import { Alert, Badge, Button, Group, Paper, SimpleGrid, Skeleton, Stack, Text, ThemeIcon, Title } from '@mantine/core'
import {
  mdiAccountMultipleOutline,
  mdiCashMultiple,
  mdiHandHeart,
  mdiMessageTextOutline,
  mdiOpenInNew,
  mdiPodium,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import api from '@Api'
import classes from '@Styles/DonationPanel.module.css'

interface DonationPanelProps {
  donateUrl?: string | null
}

const DonationPanel: FC<DonationPanelProps> = ({ donateUrl }) => {
  const { t } = useTranslation()
  const { data, error } = api.info.useInfoGetDonations({
    refreshInterval: 5 * 60 * 1000,
    revalidateOnFocus: false,
    revalidateOnReconnect: true,
    shouldRetryOnError: false,
  })
  const currency = useMemo(
    () =>
      new Intl.NumberFormat('id-ID', {
        style: 'currency',
        currency: data?.currency ?? 'IDR',
        maximumFractionDigits: 0,
      }),
    [data?.currency]
  )
  const date = useMemo(() => new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }), [])

  return (
    <Paper
      component="section"
      aria-labelledby="donation-panel-title"
      withBorder
      p={{ base: 'md', sm: 'lg' }}
      className={classes.panel}
    >
      <Group justify="space-between" align="center" mb="md">
        <Group gap="sm">
          <ThemeIcon variant="light" size="lg" radius="md" color="pink">
            <Icon path={mdiHandHeart} size={0.9} aria-hidden="true" />
          </ThemeIcon>
          <div>
            <Text size="xs" c="dimmed" fw={750} tt="uppercase" className={classes.eyebrow}>
              {t('common.content.donations.eyebrow', 'Community support')}
            </Text>
            <Title id="donation-panel-title" order={2} size="h3">
              {t('common.content.donations.title', 'Supporter wall')}
            </Title>
          </div>
        </Group>
        <Group gap="xs">
          {donateUrl && (
            <Button
              component="a"
              href={donateUrl}
              target="_blank"
              rel="noopener noreferrer"
              color="pink"
              size="sm"
              rightSection={<Icon path={mdiOpenInNew} size={0.7} aria-hidden="true" />}
              aria-label={t('common.content.donations.donate_external', 'Donate on Trakteer (opens in a new tab)')}
            >
              {t('common.content.donations.donate', 'Donate on Trakteer')}
            </Button>
          )}
          <Badge variant="light" color="pink">
            {data?.provider ?? 'Trakteer'}
          </Badge>
        </Group>
      </Group>

      {error ? (
        <Alert color="gray" title={t('common.content.donations.unavailable_title', 'Supporter wall unavailable')}>
          {t('common.content.donations.unavailable', 'Please check back later.')}
        </Alert>
      ) : !data ? (
        <SimpleGrid cols={{ base: 1, md: 2 }}>
          <Skeleton h={180} radius="md" />
          <Skeleton h={180} radius="md" />
        </SimpleGrid>
      ) : (
        <Stack gap="lg">
          <SimpleGrid
            component="section"
            cols={{ base: 1, xs: 2 }}
            spacing="sm"
            aria-label={t('common.content.donations.summary')}
          >
            <Paper withBorder p="md" className={classes.summaryCard}>
              <Group gap="sm" wrap="nowrap">
                <ThemeIcon variant="light" color="pink" radius="md">
                  <Icon path={mdiCashMultiple} size={0.82} aria-hidden="true" />
                </ThemeIcon>
                <div>
                  <Text size="xs" c="dimmed">
                    {t('common.content.donations.total_received', 'Successful support total')}
                  </Text>
                  <Text fw={750}>{currency.format(data.totalAmount)}</Text>
                </div>
              </Group>
            </Paper>
            <Paper withBorder p="md" className={classes.summaryCard}>
              <Group gap="sm" wrap="nowrap">
                <ThemeIcon variant="light" color="pink" radius="md">
                  <Icon path={mdiAccountMultipleOutline} size={0.82} aria-hidden="true" />
                </ThemeIcon>
                <div>
                  <Text size="xs" c="dimmed">
                    {t('common.content.donations.history_coverage', 'Complete support history')}
                  </Text>
                  <Text fw={750}>
                    {t(
                      'common.content.donations.history_counts',
                      '{{supports}} supports from {{supporters}} supporters',
                      {
                        supports: data.supportCount,
                        supporters: data.supporterCount,
                      }
                    )}
                  </Text>
                </div>
              </Group>
            </Paper>
          </SimpleGrid>

          <Text size="xs" c="dimmed">
            {t(
              'common.content.donations.balance_note',
              'This is the gross successful-support total. The current Trakteer balance can differ after fees and withdrawals.'
            )}
          </Text>

          <SimpleGrid cols={{ base: 1, md: 2 }} spacing="lg">
            <section aria-labelledby="donation-leaderboard-title">
              <Group gap="xs" mb="sm">
                <Icon path={mdiPodium} size={0.85} aria-hidden="true" />
                <Text id="donation-leaderboard-title" fw={700}>
                  {t('common.content.donations.leaderboard', 'Top supporters')}
                </Text>
              </Group>
              {data.leaderboard.length === 0 ? (
                <Text size="sm" c="dimmed">
                  {t('common.content.donations.empty', 'No successful support is available yet.')}
                </Text>
              ) : (
                <Stack
                  component="ol"
                  gap={0}
                  className={classes.leaderboard}
                  aria-label={t('common.content.donations.leaderboard', 'Top supporters')}
                >
                  {data.leaderboard.map((supporter) => (
                    <Group
                      component="li"
                      key={`${supporter.rank}-${supporter.supporterName}`}
                      justify="space-between"
                      wrap="nowrap"
                      className={classes.leaderboardRow}
                    >
                      <Group gap="sm" wrap="nowrap" className={classes.supporterIdentity}>
                        <Text
                          span
                          className={classes.rank}
                          aria-label={t('common.content.donations.rank', 'Rank {{rank}}', { rank: supporter.rank })}
                        >
                          {supporter.rank}
                        </Text>
                        <div className={classes.supporterText}>
                          <Text fw={650} truncate>
                            {supporter.supporterName}
                          </Text>
                          <Text size="xs" c="dimmed">
                            {t('common.content.donations.support_count', '{{count}} support', {
                              count: supporter.supportCount,
                            })}
                          </Text>
                        </div>
                      </Group>
                      <Text size="sm" fw={700} c="pink" className={classes.amount}>
                        {currency.format(supporter.totalAmount)}
                      </Text>
                    </Group>
                  ))}
                </Stack>
              )}
            </section>

            <section aria-labelledby="donation-messages-title">
              <Group gap="xs" mb="sm">
                <Icon path={mdiMessageTextOutline} size={0.85} aria-hidden="true" />
                <Text id="donation-messages-title" fw={700}>
                  {t('common.content.donations.messages', 'Supporter messages')}
                </Text>
              </Group>
              {data.messages.length === 0 ? (
                <Text size="sm" c="dimmed">
                  {t('common.content.donations.no_messages', 'No public messages yet.')}
                </Text>
              ) : (
                <Stack gap="sm" className={classes.messages}>
                  {data.messages.map((message, index) => (
                    <Paper
                      key={`${message.supporterName}-${message.updatedAt}-${index}`}
                      withBorder
                      p="sm"
                      className={classes.message}
                    >
                      <Group justify="space-between" gap="xs" align="baseline">
                        <Text fw={650} size="sm">
                          {message.supporterName}
                        </Text>
                        <Text size="xs" c="dimmed">
                          {currency.format(message.amount)}
                        </Text>
                      </Group>
                      <Text size="sm" className={classes.messageText}>
                        {message.message}
                      </Text>
                      {message.replyMessage && (
                        <Text size="xs" c="dimmed" className={classes.reply}>
                          {t('common.content.donations.reply', 'Reply')}: {message.replyMessage}
                        </Text>
                      )}
                      <Text component="time" dateTime={new Date(message.updatedAt).toISOString()} size="xs" c="dimmed">
                        {date.format(message.updatedAt)}
                      </Text>
                    </Paper>
                  ))}
                </Stack>
              )}
            </section>
          </SimpleGrid>
        </Stack>
      )}
    </Paper>
  )
}

export default DonationPanel
