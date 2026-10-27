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
  Menu,
  Paper,
  ScrollArea,
  SimpleGrid,
  Stack,
  Switch,
  Table,
  Text,
  TextInput,
} from '@mantine/core'
import { useClipboard, useInputState } from '@mantine/hooks'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import { mdiAccountMultiplePlus, mdiArrowLeftBold, mdiArrowRightBold, mdiCheck, mdiHistory, mdiMagnify } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { RequestError } from '@Components/RequestError'
import { ScrollingText } from '@Components/ScrollingText'
import { AdminPage } from '@Components/admin/AdminPage'
import { UserActions } from '@Components/admin/UserActions'
import { UserEditModal, RoleColorMap } from '@Components/admin/UserEditModal'
import { UserImportHistoryModal } from '@Components/admin/UserImportHistoryModal'
import { UserImportModal } from '@Components/admin/UserImportModal'
import {
  clearAdminPasswordResetOperation,
  retainAdminPasswordResetOperation,
} from '@Utils/AdminPasswordResetOperations'
import { isRetryableHttpError } from '@Utils/HttpError'
import { showErrorMsg } from '@Utils/Shared'
import { useArrayResponse } from '@Hooks/useArrayResponse'
import { useUser } from '@Hooks/useUser'
import api, { Role, UserInfoModel } from '@Api'
import classes from '@Styles/AdminUsers.module.css'
import tableClasses from '@Styles/Table.module.css'
import mobileClasses from './AdminMobileList.module.css'

const ITEM_COUNT_PER_PAGE = 20

const Users: FC = () => {
  const [page, setPage] = useState(1)
  const [update, setUpdate] = useState(new Date())
  const [editModalOpened, setEditModalOpened] = useState(false)
  const [importModalOpened, setImportModalOpened] = useState(false)
  const [importHistoryOpened, setImportHistoryOpened] = useState(false)
  const [activeUser, setActiveUser] = useState<UserInfoModel>({})
  const { data: users, total, setData: setUsers, updateData: updateUsers } = useArrayResponse<UserInfoModel>()
  const [hint, setHint] = useInputState('')
  const [searching, setSearching] = useState(false)
  const [loading, setLoading] = useState(true)
  const [readError, setReadError] = useState<unknown>(null)
  const searchOwner = useRef(false)
  const [disabled, setDisabled] = useState(false)
  const [current, setCurrent] = useState(0)

  const modals = useModals()
  const { user: currentUser } = useUser()
  const clipboard = useClipboard()
  const { t } = useTranslation()
  const viewport = useRef<HTMLDivElement>(null)
  const resetOperations = useRef(new Map<string, string>())
  const resetInFlight = useRef(new Set<string>())
  const setupEmailOperations = useRef(new Map<string, string>())

  useEffect(() => {
    viewport.current?.scrollTo({ top: 0, behavior: 'smooth' })
  }, [page, viewport])

  useEffect(() => {
    const fetchData = async () => {
      setLoading(true)
      try {
        const res = await api.admin.adminUsers({
          count: ITEM_COUNT_PER_PAGE,
          skip: (page - 1) * ITEM_COUNT_PER_PAGE,
        })
        setUsers(res.data)
        setCurrent((page - 1) * ITEM_COUNT_PER_PAGE + res.data.length)
        setReadError(null)
      } catch (err) {
        setReadError(err)
      } finally {
        setLoading(false)
      }
    }

    fetchData()
  }, [page, update])

  const onSearch = async () => {
    if (searchOwner.current) return
    searchOwner.current = true
    setSearching(true)
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
      setReadError(null)
    } catch (e) {
      setReadError(e)
    } finally {
      searchOwner.current = false
      setSearching(false)
    }
  }

  const onToggleEmailVerified = async (user: UserInfoModel) => {
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

  const onEditImportedUser = async (userId: string) => {
    try {
      const response = await api.admin.adminUserInfo(userId)
      const profile = response.data
      setActiveUser({
        id: profile.userId,
        userName: profile.userName,
        email: profile.email,
        bio: profile.bio,
        phone: profile.phone,
        realName: profile.realName,
        stdNumber: profile.stdNumber,
        avatar: profile.avatar,
        role: profile.role,
        ip: '',
      })
      setImportHistoryOpened(false)
      setEditModalOpened(true)
    } catch (error) {
      showErrorMsg(error, t)
    }
  }

  const onResetPassword = async (user: UserInfoModel) => {
    if (!user.id || !currentUser?.userId || resetInFlight.current.has(user.id)) return
    resetInFlight.current.add(user.id)
    const operationId = retainAdminPasswordResetOperation(
      sessionStorage,
      currentUser.userId,
      user.id,
      resetOperations.current.get(user.id) ?? null
    )
    resetOperations.current.set(user.id, operationId)
    setDisabled(true)
    try {
      const res = await api.admin.adminResetPassword(user.id, operationId)
      resetOperations.current.delete(user.id)
      clearAdminPasswordResetOperation(sessionStorage, currentUser.userId, user.id, operationId)

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
      if (!isRetryableHttpError(err)) {
        resetOperations.current.delete(user.id)
        clearAdminPasswordResetOperation(sessionStorage, currentUser.userId, user.id, operationId)
      }
      showErrorMsg(err, t)
    } finally {
      resetInFlight.current.delete(user.id)
      setDisabled(false)
    }
  }

  // Re-send a safe password setup link. Unlike temporary import credentials,
  // this works after the one-hour credential window and does not change the
  // current password unless the recipient completes the link.
  const onSendPasswordSetupEmail = async (user: UserInfoModel) => {
    if (!user.id || !user.email) return
    const operationId = setupEmailOperations.current.get(user.id) ?? crypto.randomUUID()
    setupEmailOperations.current.set(user.id, operationId)
    setDisabled(true)
    try {
      await api.admin.adminSendPasswordSetupEmail(
        user.id,
        { operationId },
        {
          headers: { 'Idempotency-Key': operationId },
        }
      )
      setupEmailOperations.current.delete(user.id)
      showNotification({
        message: t('interface.users.email_queued', {
          email: user.email,
        }),
        color: 'teal',
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (err: any) {
      if (!isRetryableHttpError(err)) setupEmailOperations.current.delete(user.id)
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

  const actionsFor = (user: UserInfoModel) => (
    <UserActions
      user={user}
      disabled={disabled}
      canDelete={user.id !== currentUser?.userId}
      onEdit={() => {
        setActiveUser(user)
        setEditModalOpened(true)
      }}
      onEmail={() => onSendPasswordSetupEmail(user)}
      onReset={() => onResetPassword(user)}
      onDelete={() => onDelete(user)}
    />
  )

  return (
    <AdminPage
      isLoading={searching || (loading && !users)}
      head={
        <div className={classes.tools}>
          <form
            className={classes.search}
            onSubmit={(event) => {
              event.preventDefault()
              void onSearch()
            }}
          >
            <TextInput
              id="admin-user-search"
              aria-label={t('interface.users.search')}
              placeholder={t('interface.users.search')}
              value={hint}
              onChange={setHint}
              rightSectionWidth={44}
              rightSection={
                <ActionIcon
                  type="submit"
                  size={44}
                  variant="subtle"
                  loading={searching}
                  aria-label={t('interface.users.search')}
                >
                  <Icon path={mdiMagnify} size={0.9} aria-hidden="true" />
                </ActionIcon>
              }
            />
          </form>
          <Menu position="bottom-end" withinPortal withInitialFocusPlaceholder={false}>
            <Menu.Target>
              <Button
                variant="default"
                mih={44}
                leftSection={<Icon path={mdiAccountMultiplePlus} size={0.85} aria-hidden="true" />}
              >
                {t('interface.users.import')}
              </Button>
            </Menu.Target>
            <Menu.Dropdown>
              <Menu.Item onClick={() => setImportModalOpened(true)}>{t('interface.users.import_csv')}</Menu.Item>
              <Menu.Item
                leftSection={<Icon path={mdiHistory} size={0.8} aria-hidden="true" />}
                onClick={() => setImportHistoryOpened(true)}
              >
                {t('interface.users.import_history')}
              </Menu.Item>
            </Menu.Dropdown>
          </Menu>
        </div>
      }
    >
      {readError != null && (
        <RequestError
          name={t('common.label.user')}
          stale={Boolean(users)}
          retrying={searching || loading}
          onRetry={onSearch}
        />
      )}
      <Paper shadow="md" p="xs" w="100%">
        <Box visibleFrom="lg">
          <ScrollArea viewportRef={viewport} offsetScrollbars scrollbarSize={4} h="calc(100vh - 190px)">
            <Table className={tableClasses.table}>
              <Table.Caption className="app-sr-only">
                {t('admin.content.users.table_caption', 'Registered users')}
              </Table.Caption>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th scope="col" miw="1.8rem">
                    {t('interface.users.email_verified')}
                  </Table.Th>
                  <Table.Th scope="col">{t('common.label.user')}</Table.Th>
                  <Table.Th scope="col">{t('account.label.email')}</Table.Th>
                  <Table.Th scope="col">{t('common.label.ip')}</Table.Th>
                  <Table.Th scope="col">{t('account.label.real_name')}</Table.Th>
                  <Table.Th scope="col">{t('account.label.student_id')}</Table.Th>
                  <Table.Th scope="col">
                    <span className="app-sr-only">{t('common.label.action', 'Actions')}</span>
                  </Table.Th>
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {users &&
                  users.map((user) => (
                    <Table.Tr key={user.id}>
                      <Table.Td>
                        <Switch
                          disabled={disabled}
                          title={t('interface.users.verification_hint')}
                          checked={user.emailConfirmed ?? false}
                          aria-label={t('interface.users.toggle_verification', {
                            name: user.userName,
                          })}
                          onChange={() => onToggleEmailVerified(user)}
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
                      <Table.Td align="right">{actionsFor(user)}</Table.Td>
                    </Table.Tr>
                  ))}
              </Table.Tbody>
            </Table>
          </ScrollArea>
        </Box>
        <SimpleGrid hiddenFrom="lg" cols={{ base: 1, sm: 2 }} spacing="sm" className={mobileClasses.mobileList}>
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
                        title={displayName}
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
                      <Text className={mobileClasses.detailLabel}>{t('interface.users.email_verified')}</Text>
                      <Text size="xs" c="dimmed">
                        {t('interface.users.verification_hint')}
                      </Text>
                      <Text size="sm" fw={650}>
                        {user.emailConfirmed ? t('interface.users.verified') : t('interface.users.unverified')}
                      </Text>
                    </Stack>
                    <Switch
                      h={44}
                      size="md"
                      disabled={disabled}
                      checked={user.emailConfirmed ?? false}
                      aria-label={t('interface.users.toggle_verification', {
                        name: displayName,
                      })}
                      onChange={() => onToggleEmailVerified(user)}
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
                  {actionsFor(user)}
                </Stack>
              </Card>
            )
          })}
        </SimpleGrid>
        {users && (
          <Group justify="space-between" className={classes.footer}>
            <Text size="sm">
              <Trans i18nKey="admin.content.users.stats" values={{ current, total }}>
                _<Code>_</Code>_
              </Trans>
            </Text>
            {total > ITEM_COUNT_PER_PAGE && (
              <Group role="group" gap="xs" wrap="nowrap" aria-label={t('common.pagination.label', 'Pagination')}>
                <ActionIcon
                  size={44}
                  disabled={page <= 1 || loading || searching}
                  aria-label={t('common.pagination.previous', 'Previous page')}
                  onClick={() => setPage(page - 1)}
                >
                  <Icon path={mdiArrowLeftBold} size={1} />
                </ActionIcon>
                <Text size="sm" aria-live="polite">
                  {page}
                </Text>
                <ActionIcon
                  size={44}
                  disabled={page * ITEM_COUNT_PER_PAGE >= total || loading || searching}
                  aria-label={t('common.pagination.next', 'Next page')}
                  onClick={() => setPage(page + 1)}
                >
                  <Icon path={mdiArrowRightBold} size={1} />
                </ActionIcon>
              </Group>
            )}
          </Group>
        )}
        <UserImportModal
          title=""
          opened={importModalOpened}
          onClose={() => setImportModalOpened(false)}
          onImportComplete={() => setUpdate(new Date())}
        />
        <UserImportHistoryModal
          opened={importHistoryOpened}
          onClose={() => setImportHistoryOpened(false)}
          onEditUser={onEditImportedUser}
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
