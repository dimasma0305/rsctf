import {
  Alert,
  Badge,
  Button,
  Center,
  Code,
  Container,
  Group,
  Paper,
  ScrollArea,
  Stack,
  Table,
  Text,
  Title,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiAlertCircle, mdiCheck, mdiRefresh, mdiShieldCheck } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import relativeTime from 'dayjs/plugin/relativeTime'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { ActionIconWithConfirm } from '@Components/ActionIconWithConfirm'
import { AdminPage } from '@Components/admin/AdminPage'
import { antiCheatExemptionState } from '@Utils/AntiCheat'
import { showErrorMsg } from '@Utils/Shared'
import api, { AntiCheatBlockModel } from '@Api'

dayjs.extend(relativeTime)

const AntiCheat: FC = () => {
  const { t } = useTranslation()
  const { data: blocks, error, isValidating, mutate } = api.admin.useAdminListAntiCheatBlocks({ count: 200 })

  const onAllow = async (b: AntiCheatBlockModel) => {
    try {
      await api.admin.adminClearAntiCheatBlock(b.id)
      await mutate()
      showNotification({
        color: 'teal',
        message: t('admin.notification.anti_cheat.exemption_granted', 'Exemption granted'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      showErrorMsg(e, t)
    }
  }

  return (
    <AdminPage isLoading={!blocks && !error}>
      <Container size="xl" mt="md">
        <Stack gap="lg">
          <Group justify="space-between" align="flex-start" gap="md" wrap="wrap">
            <Stack gap={0}>
              <Title order={2}>{t('admin.content.anti_cheat.title')}</Title>
              <Text c="dimmed">{t('admin.content.anti_cheat.subtitle')}</Text>
              <Text size="xs" c="dimmed">
                {t(
                  'admin.content.anti_cheat.limit_note',
                  'Showing the 200 most recent conflict events. Rows remain as audit history after review.'
                )}
              </Text>
            </Stack>
            <Button
              size="xs"
              variant="outline"
              leftSection={<Icon path={mdiRefresh} size={0.7} aria-hidden />}
              onClick={() => void mutate()}
              loading={isValidating}
            >
              {t('common.button.refresh', 'Refresh')}
            </Button>
          </Group>

          <Alert color="blue" variant="light" icon={<Icon path={mdiAlertCircle} size={1} aria-hidden />}>
            {t(
              'admin.content.anti_cheat.probabilistic_note',
              'IP and browser-fingerprint matches are probabilistic signals, not proof of account sharing. Shared devices, privacy settings, proxies, and NAT can cause legitimate overlap.'
            )}
          </Alert>

          {error && blocks && (
            <Alert
              color="yellow"
              role="alert"
              icon={<Icon path={mdiAlertCircle} size={1} aria-hidden />}
              title={t(
                'admin.content.anti_cheat.refresh_failed_title',
                'Refresh failed — showing the last conflict history'
              )}
            >
              {error.title ?? t('admin.content.anti_cheat.load_failed', 'The anti-cheat blocks could not be loaded.')}
            </Alert>
          )}

          {error && !blocks ? (
            <Alert
              color="red"
              variant="light"
              icon={<Icon path={mdiAlertCircle} size={1} />}
              title={t('admin.content.anti_cheat.load_failed_title', 'Failed to load anti-cheat blocks')}
            >
              <Stack gap="sm" align="flex-start">
                <Text size="sm">
                  {error.title ??
                    t('admin.content.anti_cheat.load_failed', 'The anti-cheat blocks could not be loaded.')}
                </Text>
                <Button size="xs" variant="outline" color="red" onClick={() => void mutate()}>
                  {t('admin.button.anti_cheat.retry', 'Retry')}
                </Button>
              </Stack>
            </Alert>
          ) : !blocks || blocks.length === 0 ? (
            <Center h="30vh" role="status">
              <Stack gap={0} align="center">
                <Title order={3} size="h4">
                  {t('admin.content.anti_cheat.empty_title')}
                </Title>
                <Text c="dimmed">{t('admin.content.anti_cheat.empty')}</Text>
              </Stack>
            </Center>
          ) : (
            <Paper p="xs" withBorder>
              <ScrollArea
                viewportProps={{
                  role: 'region',
                  tabIndex: 0,
                  'aria-label': t('admin.content.anti_cheat.scroll_region', 'Anti-cheat conflict history'),
                }}
              >
                <Table withTableBorder striped highlightOnHover>
                  <Table.Caption>{t('admin.content.anti_cheat.table_caption', 'Anti-cheat conflicts')}</Table.Caption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col">{t('admin.content.anti_cheat.column.when')}</Table.Th>
                      <Table.Th scope="col">{t('admin.content.anti_cheat.column.user')}</Table.Th>
                      <Table.Th scope="col">{t('admin.content.anti_cheat.column.kind')}</Table.Th>
                      <Table.Th scope="col">{t('admin.content.anti_cheat.column.conflict_with')}</Table.Th>
                      <Table.Th scope="col">{t('admin.content.anti_cheat.column.value')}</Table.Th>
                      <Table.Th scope="col">
                        {t('admin.content.anti_cheat.column.exemption', 'Review / exemption')}
                      </Table.Th>
                      <Table.Th scope="col">
                        <span className="app-sr-only">{t('common.label.action', 'Actions')}</span>
                      </Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {blocks.map((b) => {
                      const exemptionState = antiCheatExemptionState(b)
                      return (
                        <Table.Tr key={b.id}>
                          <Table.Td>
                            <Stack gap={0}>
                              <Text size="sm">{dayjs(b.occurredAtUtc).fromNow()}</Text>
                              <Text size="xs" c="dimmed" ff="monospace">
                                {dayjs(b.occurredAtUtc).format('YYYY-MM-DD HH:mm')}
                              </Text>
                            </Stack>
                          </Table.Td>
                          <Table.Td>
                            <Text size="sm" fw="bold">
                              {b.userName ?? '—'}
                            </Text>
                          </Table.Td>
                          <Table.Td>
                            <Badge size="sm" color={b.kind === 'Ip' ? 'blue' : 'violet'} variant="light">
                              {b.kind === 'Ip'
                                ? t('admin.content.anti_cheat.kind.ip', 'IP match')
                                : t('admin.content.anti_cheat.kind.fingerprint', 'Browser fingerprint match')}
                            </Badge>
                          </Table.Td>
                          <Table.Td>
                            <Text size="sm">{b.conflictUserName ?? '—'}</Text>
                          </Table.Td>
                          <Table.Td>
                            {b.conflictingValue ? (
                              <Code title={t('admin.content.anti_cheat.masked_hint', 'Masked correlation hint')}>
                                {b.conflictingValue}
                              </Code>
                            ) : (
                              '—'
                            )}
                          </Table.Td>
                          <Table.Td>
                            <Stack gap={2}>
                              <Badge
                                size="sm"
                                variant="light"
                                color={
                                  exemptionState === 'active'
                                    ? 'green'
                                    : exemptionState === 'expired'
                                      ? 'yellow'
                                      : 'gray'
                                }
                              >
                                {exemptionState === 'active'
                                  ? t('admin.content.anti_cheat.exemption.active', 'Active exemption')
                                  : exemptionState === 'expired'
                                    ? t('admin.content.anti_cheat.exemption.expired', 'Exemption expired')
                                    : t('admin.content.anti_cheat.exemption.unreviewed', 'Not adjudicated')}
                              </Badge>
                              {b.exemptionExpiresAtUtc != null && (
                                <Text size="xs" c="dimmed">
                                  {exemptionState === 'active'
                                    ? t('admin.content.anti_cheat.exemption.until', 'Until {{time}}', {
                                        time: dayjs(b.exemptionExpiresAtUtc).format('YYYY-MM-DD HH:mm'),
                                      })
                                    : t('admin.content.anti_cheat.exemption.ended', 'Ended {{time}}', {
                                        time: dayjs(b.exemptionExpiresAtUtc).format('YYYY-MM-DD HH:mm'),
                                      })}
                                </Text>
                              )}
                            </Stack>
                          </Table.Td>
                          <Table.Td align="right">
                            <ActionIconWithConfirm
                              iconPath={mdiShieldCheck}
                              color="teal"
                              message={t(
                                exemptionState === 'active'
                                  ? 'admin.content.anti_cheat.exemption.already_active'
                                  : 'admin.content.anti_cheat.allow_confirm',
                                exemptionState === 'active'
                                  ? 'A 7-day exemption is already active for this exact match.'
                                  : 'Allow this exact account and identity match for 7 days? The conflict remains in the audit history.'
                              )}
                              disabled={exemptionState === 'active'}
                              onClick={() => onAllow(b)}
                            />
                          </Table.Td>
                        </Table.Tr>
                      )
                    })}
                  </Table.Tbody>
                </Table>
              </ScrollArea>
            </Paper>
          )}
        </Stack>
      </Container>
    </AdminPage>
  )
}

export default AntiCheat
