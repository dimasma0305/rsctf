import {
  Alert,
  Badge,
  Button,
  Center,
  Code,
  Container,
  Paper,
  ScrollArea,
  Stack,
  Table,
  Text,
  Title,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiAlertCircle, mdiCheck, mdiDeleteOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import relativeTime from 'dayjs/plugin/relativeTime'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { ActionIconWithConfirm } from '@Components/ActionIconWithConfirm'
import { AdminPage } from '@Components/admin/AdminPage'
import { showErrorMsg } from '@Utils/Shared'
import api, { AntiCheatBlockModel } from '@Api'

dayjs.extend(relativeTime)

const AntiCheat: FC = () => {
  const { t } = useTranslation()
  const { data: blocks, error, mutate } = api.admin.useAdminListAntiCheatBlocks({ count: 200 })

  const onClear = async (b: AntiCheatBlockModel) => {
    try {
      await api.admin.adminClearAntiCheatBlock(b.id)
      showNotification({
        color: 'teal',
        message: t('admin.notification.anti_cheat.cleared'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutate()
    } catch (e) {
      showErrorMsg(e, t)
    }
  }

  return (
    <AdminPage isLoading={!blocks && !error}>
      <Container size="xl" mt="md">
        <Stack gap="lg">
          <Stack gap={0}>
            <Title order={2}>{t('admin.content.anti_cheat.title')}</Title>
            <Text c="dimmed">{t('admin.content.anti_cheat.subtitle')}</Text>
          </Stack>

          {error ? (
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
                <Button size="xs" variant="outline" color="red" onClick={() => mutate()}>
                  {t('admin.button.anti_cheat.retry', 'Retry')}
                </Button>
              </Stack>
            </Alert>
          ) : !blocks || blocks.length === 0 ? (
            <Center h="30vh">
              <Stack gap={0} align="center">
                <Title order={4}>{t('admin.content.anti_cheat.empty_title')}</Title>
                <Text c="dimmed">{t('admin.content.anti_cheat.empty')}</Text>
              </Stack>
            </Center>
          ) : (
            <Paper p="xs" withBorder>
              <ScrollArea>
                <Table withTableBorder striped highlightOnHover>
                  <Table.Caption>{t('admin.content.anti_cheat.table_caption', 'Anti-cheat conflicts')}</Table.Caption>
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th scope="col">{t('admin.content.anti_cheat.column.when')}</Table.Th>
                      <Table.Th scope="col">{t('admin.content.anti_cheat.column.user')}</Table.Th>
                      <Table.Th scope="col">{t('admin.content.anti_cheat.column.kind')}</Table.Th>
                      <Table.Th scope="col">{t('admin.content.anti_cheat.column.conflict_with')}</Table.Th>
                      <Table.Th scope="col">{t('admin.content.anti_cheat.column.value')}</Table.Th>
                      <Table.Th scope="col" aria-label={t('common.label.action', 'Actions')} />
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {blocks.map((b) => (
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
                          <Badge size="sm" color={b.kind === 'Ip' ? 'blue' : 'orange'} variant="light">
                            {b.kind}
                          </Badge>
                        </Table.Td>
                        <Table.Td>
                          <Text size="sm">{b.conflictUserName ?? '—'}</Text>
                        </Table.Td>
                        <Table.Td>
                          {b.conflictingValue ? (
                            <Code>
                              {b.kind === 'Fingerprint'
                                ? b.conflictingValue.substring(0, 16) + '…'
                                : b.conflictingValue}
                            </Code>
                          ) : (
                            '—'
                          )}
                        </Table.Td>
                        <Table.Td align="right">
                          <ActionIconWithConfirm
                            iconPath={mdiDeleteOutline}
                            color="red"
                            message={t(
                              'admin.content.anti_cheat.clear_confirm',
                              'Clear this anti-cheat block? This cannot be undone.'
                            )}
                            onClick={() => onClear(b)}
                          />
                        </Table.Td>
                      </Table.Tr>
                    ))}
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
