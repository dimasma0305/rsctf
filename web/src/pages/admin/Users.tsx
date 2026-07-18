import {
  ActionIcon,
  Avatar,
  Badge,
  Box,
  Button,
  Card,
  Code,
  Divider,
  Group,
  Paper,
  ScrollArea,
  Stack,
  Switch,
  Table,
  Text,
  TextInput,
} from '@mantine/core'
import { useClipboard, useInputState } from '@mantine/hooks'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import {
  mdiAccountMultiplePlus,
  mdiAccountOutline,
  mdiAlertCircle,
  mdiArrowLeftBold,
  mdiArrowRightBold,
  mdiCheck,
  mdiDeleteOutline,
  mdiEmailArrowRightOutline,
  mdiLockReset,
  mdiMagnify,
  mdiPencilOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { ActionIconWithConfirm } from '@Components/ActionIconWithConfirm'
import { ScrollingText } from '@Components/ScrollingText'
import { AdminPage } from '@Components/admin/AdminPage'
import { UserEditModal, RoleColorMap } from '@Components/admin/UserEditModal'
import { UserImportModal } from '@Components/admin/UserImportModal'
import { showErrorMsg } from '@Utils/Shared'
import { useArrayResponse } from '@Hooks/useArrayResponse'
import { useUser } from '@Hooks/useUser'
import api, { Role, UserInfoModel } from '@Api'
import tableClasses from '@Styles/Table.module.css'
import mobileClasses from './AdminMobileList.module.css'

const ITEM_COUNT_PER_PAGE = 30

const Users: FC = () => {
  const [page, setPage] = useState(1)
  const [update, setUpdate] = useState(new Date())
  const [editModalOpened, setEditModalOpened] = useState(false)
  const [importModalOpened, setImportModalOpened] = useState(false)
  const [activeUser, setActiveUser] = useState<UserInfoModel>({})
  const { data: users, total, setData: setUsers, updateData: updateUsers } = useArrayResponse<UserInfoModel>()
  const [hint, setHint] = useInputState('')
  const [searching, setSearching] = useState(false)
  const [disabled, setDisabled] = useState(false)
  const [current, setCurrent] = useState(0)

  const modals = useModals()
  const { user: currentUser } = useUser()
  const clipboard = useClipboard()
  const { t } = useTranslation()
  const viewport = useRef<HTMLDivElement>(null)

  useEffect(() => {
    viewport.current?.scrollTo({ top: 0, behavior: 'smooth' })
  }, [page, viewport])

  useEffect(() => {
    const fetchData = async () => {
      try {
        const res = await api.admin.adminUsers({
          count: ITEM_COUNT_PER_PAGE,
          skip: (page - 1) * ITEM_COUNT_PER_PAGE,
        })
        setUsers(res.data)
        setCurrent((page - 1) * ITEM_COUNT_PER_PAGE + res.data.length)
      } catch (err) {
        showErrorMsg(err, t)
      }
    }

    fetchData()
  }, [page, update])

  const onSearch = async () => {
    try {
      if (!hint) {
        const res = await api.admin.adminUsers({
          count: ITEM_COUNT_PER_PAGE,
          skip: (page - 1) * ITEM_COUNT_PER_PAGE,
        })
        setUsers(res.data)
        setCurrent((page - 1) * ITEM_COUNT_PER_PAGE + res.data.length)
      } else {
        const res = await api.admin.adminSearchUsers({ hint })
        setUsers(res.data)
        setCurrent(res.data.length)
      }
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setSearching(false)
    }
  }

  const onToggleActive = async (user: UserInfoModel) => {
    setDisabled(true)

    try {
      await api.admin.adminUpdateUserInfo(user.id!, {
        emailConfirmed: !user.emailConfirmed,
      })
      if (users) {
        updateUsers(
          users.map((u) =>
            u.id === user.id
              ? {
                  ...u,
                  emailConfirmed: !u.emailConfirmed,
                }
              : u
          )
        )
      }
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const onResetPassword = async (user: UserInfoModel) => {
    setDisabled(true)
    try {
      const res = await api.admin.adminResetPassword(user.id!)

      modals.openModal({
        title: t('admin.content.users.reset.title', {
          name: user.userName,
        }),

        children: (
          <Stack>
            <Text>
              <Trans i18nKey="admin.content.users.reset.content" />
            </Text>
            <ScrollingText text={res.data} fw="bold" maw="25rem" />
            <Button
              onClick={() => {
                clipboard.copy(res.data)
                showNotification({
                  message: t('admin.notification.users.password_copied'),
                  color: 'teal',
                  icon: <Icon path={mdiCheck} size={1} />,
                })
              }}
            >
              {t('common.button.copy')}
            </Button>
          </Stack>
        ),
      })
    } catch (err: any) {
      showErrorMsg(err, t)
    } finally {
      setDisabled(false)
    }
  }

  // Send (or re-send) a "set your password" email to a single user — the
  // per-user counterpart of the bulk credential send in the import modal.
  // Works any time (e.g. after an admin changes a user's email), reusing the
  // same endpoint with a one-item list; the server returns a per-recipient
  // result so we can show the exact failure reason if SMTP rejects it.
  const onSendCredentials = async (user: UserInfoModel) => {
    if (!user.email) return
    setDisabled(true)
    try {
      const resp = await fetch('/api/admin/users/credentials/send', {
        method: 'POST',
        credentials: 'include',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ items: [{ email: user.email, userName: user.userName ?? '' }] }),
      })
      if (!resp.ok) {
        const err = await resp.json().catch(() => ({ title: undefined }))
        throw new Error(err.title ?? t('admin.notification.users.credentials_failed', 'Failed to send email'))
      }
      const result: { sent: number; failed: number; results?: { error: string | null }[] } = await resp.json()
      if (result.sent > 0) {
        showNotification({
          message: t('admin.notification.users.credentials_sent', 'Password-setup email sent to {{email}}', {
            email: user.email,
          }),
          color: 'teal',
          icon: <Icon path={mdiCheck} size={1} />,
        })
      } else {
        const reason = result.results?.[0]?.error
        showNotification({
          message: reason
            ? t('admin.notification.users.credentials_failed_reason', 'Failed to send: {{reason}}', { reason })
            : t('admin.notification.users.credentials_failed', 'Failed to send email'),
          color: 'red',
          icon: <Icon path={mdiAlertCircle} size={1} />,
        })
      }
    } catch (err: any) {
      showErrorMsg(err, t)
    } finally {
      setDisabled(false)
    }
  }

  const onDelete = async (user: UserInfoModel) => {
    try {
      setDisabled(true)
      if (!user.id) return

      await api.admin.adminDeleteUser(user.id)
      showNotification({
        message: t('admin.notification.users.deleted', {
          name: user.userName,
        }),
        color: 'teal',
        icon: <Icon path={mdiCheck} size={1} />,
      })
      if (users) {
        updateUsers(users.filter((x) => x.id !== user.id))
      }
      setCurrent(current - 1)
      setUpdate(new Date())
    } catch (e: any) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  return (
    <AdminPage
      isLoading={searching || !users}
      head={
        <>
          <TextInput
            w={{ base: '100%', sm: '36%' }}
            aria-label={t('admin.placeholder.users.search')}
            leftSection={<Icon path={mdiMagnify} size={1} />}
            placeholder={t('admin.placeholder.users.search')}
            value={hint}
            onChange={setHint}
            onKeyDown={(e) => {
              if (!searching && e.key === 'Enter') onSearch()
            }}
            rightSection={<Icon path={mdiAccountOutline} size={1} />}
          />
          <Group w={{ base: '100%', sm: 'auto' }} justify="space-between" wrap="wrap" gap="xs">
            <Button
              leftSection={<Icon path={mdiAccountMultiplePlus} size={0.9} />}
              variant="outline"
              h={44}
              onClick={() => setImportModalOpened(true)}
            >
              Import CSV
            </Button>
            <Text fw="bold" size="sm">
              <Trans
                i18nKey="admin.content.users.stats"
                values={{
                  current,
                  total,
                }}
              >
                _<Code>_</Code>_
              </Trans>
            </Text>
            <Group role="group" gap="xs" wrap="nowrap" aria-label={t('common.pagination.label', 'Pagination')}>
              <ActionIcon
                size={44}
                disabled={page <= 1}
                aria-label={t('common.pagination.previous', 'Previous page')}
                onClick={() => setPage(page - 1)}
              >
                <Icon path={mdiArrowLeftBold} size={1} />
              </ActionIcon>
              <Text fw="bold" size="sm" aria-live="polite">
                {page}
              </Text>
              <ActionIcon
                size={44}
                disabled={page * ITEM_COUNT_PER_PAGE >= total}
                aria-label={t('common.pagination.next', 'Next page')}
                onClick={() => setPage(page + 1)}
              >
                <Icon path={mdiArrowRightBold} size={1} />
              </ActionIcon>
            </Group>
          </Group>
        </>
      }
    >
      <Paper shadow="md" p="xs" w="100%">
        <Box visibleFrom="sm">
          <ScrollArea viewportRef={viewport} offsetScrollbars scrollbarSize={4} h="calc(100vh - 190px)">
            <Table className={tableClasses.table}>
              <Table.Caption className="app-sr-only">
                {t('admin.content.users.table_caption', 'Registered users')}
              </Table.Caption>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th scope="col" miw="1.8rem">
                    {t('admin.label.users.active')}
                  </Table.Th>
                  <Table.Th scope="col">{t('common.label.user')}</Table.Th>
                  <Table.Th scope="col">{t('account.label.email')}</Table.Th>
                  <Table.Th scope="col">{t('common.label.ip')}</Table.Th>
                  <Table.Th scope="col">{t('account.label.real_name')}</Table.Th>
                  <Table.Th scope="col">{t('account.label.student_id')}</Table.Th>
                  <Table.Th scope="col" aria-label={t('common.label.action', 'Actions')} />
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {users &&
                  users.map((user) => (
                    <Table.Tr key={user.id}>
                      <Table.Td>
                        <Switch
                          disabled={disabled}
                          checked={user.emailConfirmed ?? false}
                          aria-label={t('admin.label.users.toggle_active', 'Toggle active status for {{name}}', {
                            name: user.userName,
                          })}
                          onChange={() => onToggleActive(user)}
                        />
                      </Table.Td>
                      <Table.Td>
                        <Group wrap="nowrap" justify="space-between" gap="xs">
                          <Group wrap="nowrap" justify="left">
                            <Avatar imageProps={{ loading: 'lazy' }} alt="avatar" src={user.avatar} radius="xl">
                              {user.userName?.slice(0, 1) ?? 'U'}
                            </Avatar>
                            <ScrollingText text={user.userName ?? ''} ff="monospace" size="sm" fw="bold" maw="8rem" />
                          </Group>
                          <Badge size="sm" color={RoleColorMap.get(user.role ?? Role.User)}>
                            {user.role}
                          </Badge>
                        </Group>
                      </Table.Td>
                      <Table.Td>
                        <ScrollingText text={user.email ?? ''} size="sm" maw="12rem" />
                      </Table.Td>
                      <Table.Td>
                        <Text lineClamp={1} size="sm" ff="monospace">
                          {user.ip}
                        </Text>
                      </Table.Td>
                      <Table.Td>
                        <ScrollingText
                          text={user.realName ?? t('admin.placeholder.users.real_name')}
                          size="sm"
                          maw="6rem"
                        />
                      </Table.Td>
                      <Table.Td>
                        <ScrollingText
                          text={user.stdNumber ?? t('admin.placeholder.users.student_id')}
                          size="sm"
                          ff="monospace"
                          maw="8rem"
                        />
                      </Table.Td>
                      <Table.Td align="right">
                        <Group wrap="nowrap" gap="sm" justify="right">
                          <ActionIcon
                            color="blue"
                            aria-label={t('admin.button.users.edit')}
                            onClick={() => {
                              setActiveUser(user)
                              setEditModalOpened(true)
                            }}
                          >
                            <Icon path={mdiPencilOutline} size={1} />
                          </ActionIcon>
                          <ActionIconWithConfirm
                            iconPath={mdiEmailArrowRightOutline}
                            color="teal"
                            message={t('admin.content.users.send_credentials', {
                              name: user.userName,
                              defaultValue: 'Send a "set your password" email to {{name}}?',
                            })}
                            disabled={disabled || !user.email}
                            onClick={() => onSendCredentials(user)}
                          />
                          <ActionIconWithConfirm
                            iconPath={mdiLockReset}
                            color="orange"
                            message={t('admin.content.users.reset.message', {
                              name: user.userName,
                            })}
                            disabled={disabled}
                            onClick={() => onResetPassword(user)}
                          />
                          <ActionIconWithConfirm
                            iconPath={mdiDeleteOutline}
                            color="alert"
                            message={t('admin.content.users.delete', {
                              name: user.userName,
                            })}
                            disabled={disabled || user.id === currentUser?.userId}
                            onClick={() => onDelete(user)}
                          />
                        </Group>
                      </Table.Td>
                    </Table.Tr>
                  ))}
              </Table.Tbody>
            </Table>
          </ScrollArea>
        </Box>
        <Stack hiddenFrom="sm" gap="sm" className={mobileClasses.mobileList}>
          {users?.map((user) => {
            const userHeadingId = `mobile-user-${user.id}`
            const displayName = user.userName || t('common.label.user')

            return (
              <Card
                component="article"
                key={user.id}
                withBorder
                radius="lg"
                p="md"
                className={mobileClasses.card}
                aria-labelledby={userHeadingId}
              >
                <Stack gap="md">
                  <Group wrap="nowrap" align="center" gap="sm">
                    <Avatar imageProps={{ loading: 'lazy' }} alt="" src={user.avatar} radius="xl" size={48}>
                      {displayName.slice(0, 1)}
                    </Avatar>
                    <Stack gap={4} className={mobileClasses.identity}>
                      <Text
                        component="h2"
                        id={userHeadingId}
                        ff="monospace"
                        size="sm"
                        fw={750}
                        className={mobileClasses.recordTitle}
                      >
                        {displayName}
                      </Text>
                      <Badge size="sm" variant="light" color={RoleColorMap.get(user.role ?? Role.User)} w="fit-content">
                        {user.role ?? Role.User}
                      </Badge>
                    </Stack>
                  </Group>

                  <Group className={mobileClasses.stateRow} justify="space-between" wrap="nowrap">
                    <Stack gap={1}>
                      <Text className={mobileClasses.detailLabel}>{t('admin.label.users.active')}</Text>
                      <Text size="sm" fw={650}>
                        {user.emailConfirmed
                          ? t('admin.label.users.active_state', 'Active')
                          : t('admin.label.users.inactive_state', 'Inactive')}
                      </Text>
                    </Stack>
                    <Switch
                      h={44}
                      size="md"
                      disabled={disabled}
                      checked={user.emailConfirmed ?? false}
                      aria-label={t('admin.label.users.toggle_active', 'Toggle active status for {{name}}', {
                        name: displayName,
                      })}
                      onChange={() => onToggleActive(user)}
                    />
                  </Group>

                  <Box component="dl" className={mobileClasses.details}>
                    <Box component="div" className={mobileClasses.detail}>
                      <Text component="dt" className={mobileClasses.detailLabel}>
                        {t('account.label.email')}
                      </Text>
                      <Text component="dd" size="sm" className={mobileClasses.detailValue}>
                        {user.email || '—'}
                      </Text>
                    </Box>
                    <Box component="div" className={mobileClasses.detail}>
                      <Text component="dt" className={mobileClasses.detailLabel}>
                        {t('common.label.ip')}
                      </Text>
                      <Text component="dd" size="sm" ff="monospace" className={mobileClasses.detailValue}>
                        {user.ip || '—'}
                      </Text>
                    </Box>
                    <Box component="div" className={mobileClasses.detail}>
                      <Text component="dt" className={mobileClasses.detailLabel}>
                        {t('account.label.real_name')}
                      </Text>
                      <Text component="dd" size="sm" className={mobileClasses.detailValue}>
                        {user.realName || t('admin.placeholder.users.real_name')}
                      </Text>
                    </Box>
                    <Box component="div" className={mobileClasses.detail}>
                      <Text component="dt" className={mobileClasses.detailLabel}>
                        {t('account.label.student_id')}
                      </Text>
                      <Text component="dd" size="sm" ff="monospace" className={mobileClasses.detailValue}>
                        {user.stdNumber || t('admin.placeholder.users.student_id')}
                      </Text>
                    </Box>
                  </Box>

                  <Divider />
                  <Box
                    component="section"
                    aria-label={t('common.label.action', 'Actions')}
                    className={mobileClasses.actionGrid}
                  >
                    <Box className={mobileClasses.actionCell}>
                      <ActionIcon
                        size={44}
                        variant="light"
                        color="blue"
                        aria-label={t('admin.button.users.edit_named', 'Edit {{name}}', { name: displayName })}
                        onClick={() => {
                          setActiveUser(user)
                          setEditModalOpened(true)
                        }}
                      >
                        <Icon path={mdiPencilOutline} size={1} />
                      </ActionIcon>
                      <span className={mobileClasses.actionLabel}>{t('admin.button.users.edit')}</span>
                    </Box>
                    <Box className={mobileClasses.actionCell}>
                      <ActionIconWithConfirm
                        iconPath={mdiEmailArrowRightOutline}
                        color="teal"
                        message={t('admin.content.users.send_credentials', {
                          name: displayName,
                          defaultValue: 'Send a "set your password" email to {{name}}?',
                        })}
                        disabled={disabled || !user.email}
                        onClick={() => onSendCredentials(user)}
                      />
                      <span className={mobileClasses.actionLabel}>
                        {t('admin.button.users.credentials', 'Credentials')}
                      </span>
                    </Box>
                    <Box className={mobileClasses.actionCell}>
                      <ActionIconWithConfirm
                        iconPath={mdiLockReset}
                        color="orange"
                        message={t('admin.content.users.reset.message', {
                          name: displayName,
                        })}
                        disabled={disabled}
                        onClick={() => onResetPassword(user)}
                      />
                      <span className={mobileClasses.actionLabel}>{t('account.button.reset')}</span>
                    </Box>
                    <Box className={mobileClasses.actionCell}>
                      <ActionIconWithConfirm
                        iconPath={mdiDeleteOutline}
                        color="alert"
                        message={t('admin.content.users.delete', {
                          name: displayName,
                        })}
                        disabled={disabled || user.id === currentUser?.userId}
                        onClick={() => onDelete(user)}
                      />
                      <span className={mobileClasses.actionLabel}>{t('common.modal.delete')}</span>
                    </Box>
                  </Box>
                </Stack>
              </Card>
            )
          })}
        </Stack>
        <UserImportModal
          title=""
          opened={importModalOpened}
          onClose={() => setImportModalOpened(false)}
          onImportComplete={() => setUpdate(new Date())}
        />
        <UserEditModal
          size="min(42rem, calc(100vw - 2rem))"
          title={t('admin.button.users.edit')}
          user={activeUser}
          opened={editModalOpened}
          onClose={() => setEditModalOpened(false)}
          mutateUser={(user: UserInfoModel) => {
            updateUsers(
              [user, ...(users?.filter((n) => n.id !== user.id) ?? [])].sort((a, b) => (a.id! < b.id! ? -1 : 1))
            )
          }}
        />
      </Paper>
    </AdminPage>
  )
}

export default Users
