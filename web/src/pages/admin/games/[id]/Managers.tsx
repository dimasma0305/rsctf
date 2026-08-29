import {
  ActionIcon,
  Avatar,
  Button,
  Group,
  Paper,
  Select,
  Stack,
  Table,
  Text,
  Loader,
  ComboboxItem,
} from '@mantine/core'
import { useDebouncedValue } from '@mantine/hooks'
import { useModals } from '@mantine/modals'
import { mdiDelete, mdiAccountPlus } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { WithGameEditTab } from '@Components/admin/WithGameEditTab'
import { requireApiCollection } from '@Utils/ApiCollection'
import {
  createLatestAutocompleteRequests,
  MANAGER_AUTOCOMPLETE_MAX_CHARS,
  MANAGER_AUTOCOMPLETE_MIN_CHARS,
  normalizeManagerAutocompleteQuery,
} from '@Utils/ManagerAutocomplete'
import { showErrorMsg, showSuccessMsg } from '@Utils/Shared'
import api, { ManagerAutocompleteUserModel, UserInfoModel } from '@Api'

export const Managers: FC = () => {
  const { id } = useParams()
  const gameId = parseInt(id ?? '0')
  const { t } = useTranslation()
  const modals = useModals()

  const [admins, setAdmins] = useState<UserInfoModel[]>()
  const [isLoadingAdmins, setIsLoadingAdmins] = useState(false)

  const [searchValue, setSearchValue] = useState('')
  const [debouncedSearch] = useDebouncedValue(searchValue, 300)
  const [selectedUser, setSelectedUser] = useState<string | null>(null)

  const [users, setUsers] = useState<ManagerAutocompleteUserModel[]>()
  const [isLoadingUsers, setIsLoadingUsers] = useState(false)
  const autocompleteRequests = useRef(createLatestAutocompleteRequests())

  const fetchAdmins = async () => {
    if (!gameId) return
    setIsLoadingAdmins(true)
    try {
      const res = await api.edit.editGetGameAdmins(gameId)
      setAdmins(requireApiCollection<UserInfoModel>(res.data, { label: 'Game manager list' }).items)
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setIsLoadingAdmins(false)
    }
  }

  useEffect(() => {
    fetchAdmins()
  }, [gameId])

  // Raw input changes invalidate immediately instead of waiting for the next
  // debounce. This is what makes clearing the field authoritative.
  useEffect(() => {
    autocompleteRequests.current.invalidate()
    setUsers(undefined)
    setIsLoadingUsers(false)
  }, [searchValue])

  useEffect(() => {
    autocompleteRequests.current.invalidate()
    setUsers(undefined)
    setIsLoadingUsers(false)
    setSelectedUser(null)
    setSearchValue('')
    return () => autocompleteRequests.current.invalidate()
  }, [gameId])

  useEffect(() => {
    const query = normalizeManagerAutocompleteQuery(debouncedSearch)
    if (!query) return

    void autocompleteRequests.current.run(
      async (signal) =>
        requireApiCollection<ManagerAutocompleteUserModel>(
          (await api.admin.adminManagerAutocomplete({ query }, { signal })).data,
          { label: 'Manager autocomplete list' }
        ).items,
      {
        setLoading: setIsLoadingUsers,
        setResults: setUsers,
        onError: (error) => showErrorMsg(error, t),
      }
    )
  }, [debouncedSearch, t])

  const handleAddAdmin = async () => {
    if (!selectedUser || !gameId) return

    try {
      await api.edit.editAddGameAdmin(gameId, selectedUser)
      showSuccessMsg(t('admin.notification.games.managers.added'))
      setSelectedUser(null)
      setSearchValue('')
      fetchAdmins()
    } catch (e: any) {
      showErrorMsg(e, t)
    }
  }

  const handleRemoveAdmin = async (userId: string) => {
    if (!gameId) return

    try {
      await api.edit.editRemoveGameAdmin(gameId, userId)
      showSuccessMsg(t('admin.notification.games.managers.removed'))
      fetchAdmins()
    } catch (e: any) {
      showErrorMsg(e, t)
    }
  }

  const onConfirmRemove = (userId: string, userName: string | null | undefined) => {
    modals.openConfirmModal({
      title: t('admin.content.games.managers.delete_title'),
      children: (
        <Text size="sm">{t('admin.content.games.managers.delete_confirm', { name: userName || 'this manager' })}</Text>
      ),
      onConfirm: () => handleRemoveAdmin(userId),
      confirmProps: { color: 'red' },
    })
  }

  const userOptions: ComboboxItem[] = (users ?? []).map((u) => ({
    value: u.id,
    label: u.email ? `${u.userName ?? u.email} (${u.email})` : (u.userName ?? u.id),
  }))
  const hasInvalidSearch = searchValue.trim().length > 0 && !normalizeManagerAutocompleteQuery(searchValue)
  const searchStatus = isLoadingUsers
    ? t('admin.content.games.managers.searching', 'Searching users')
    : users
      ? t('admin.content.games.managers.results', '{{count}} matching users', { count: users.length })
      : ''

  return (
    <WithGameEditTab isLoading={isLoadingAdmins && !admins}>
      <Stack>
        <Paper withBorder p="md">
          <Group align="flex-end" wrap="wrap">
            <Select
              label={t('admin.content.games.managers.select_user')}
              placeholder={t('admin.content.games.managers.search_placeholder')}
              data={userOptions}
              value={selectedUser}
              onChange={setSelectedUser}
              searchValue={searchValue}
              onSearchChange={setSearchValue}
              searchable
              clearable
              nothingFoundMessage={
                isLoadingUsers ? (
                  <Loader size="xs" />
                ) : hasInvalidSearch ? (
                  t('admin.content.games.managers.search_bounds', 'Enter between {{min}} and {{max}} characters', {
                    min: MANAGER_AUTOCOMPLETE_MIN_CHARS,
                    max: MANAGER_AUTOCOMPLETE_MAX_CHARS,
                  })
                ) : (
                  t('common.content.no_data')
                )
              }
              style={{ flex: '1 1 16rem' }}
              filter={({ options }) => options} // Server-side filtering
            />
            <Text component="span" className="app-sr-only" aria-live="polite">
              {searchStatus}
            </Text>
            <Button
              leftSection={<Icon path={mdiAccountPlus} size={1} />}
              onClick={handleAddAdmin}
              disabled={!selectedUser}
            >
              {t('common.button.add')}
            </Button>
          </Group>
        </Paper>

        <Paper withBorder p="0">
          <Table>
            <Table.Caption>{t('admin.content.games.managers.table_caption', 'Game managers')}</Table.Caption>
            <Table.Thead>
              <Table.Tr>
                <Table.Th scope="col">{t('common.label.user')}</Table.Th>
                <Table.Th scope="col">{t('account.label.email')}</Table.Th>
                <Table.Th scope="col" w={100}>
                  {t('common.label.action')}
                </Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {isLoadingAdmins && (
                <Table.Tr>
                  <Table.Td colSpan={3}>
                    <Group justify="center" p="md">
                      <Loader />
                    </Group>
                  </Table.Td>
                </Table.Tr>
              )}
              {admins?.map((admin) => (
                <Table.Tr key={admin.id}>
                  <Table.Td>
                    <Group gap="xs">
                      <Avatar src={admin.avatar} size="sm" radius="xl" />
                      <Text size="sm" fw={500}>
                        {admin.userName}
                      </Text>
                    </Group>
                  </Table.Td>
                  <Table.Td>{admin.email}</Table.Td>
                  <Table.Td>
                    <ActionIcon
                      color="red"
                      variant="subtle"
                      aria-label={t('admin.button.games.managers.remove', 'Remove {{name}}', {
                        name: admin.userName,
                      })}
                      onClick={() => admin.id && onConfirmRemove(admin.id, admin.userName)}
                    >
                      <Icon path={mdiDelete} size={1} />
                    </ActionIcon>
                  </Table.Td>
                </Table.Tr>
              ))}

              {!isLoadingAdmins && admins?.length === 0 && (
                <Table.Tr>
                  <Table.Td colSpan={3} ta="center">
                    <Text c="dimmed">{t('admin.content.games.managers.empty')}</Text>
                  </Table.Td>
                </Table.Tr>
              )}
            </Table.Tbody>
          </Table>
        </Paper>
      </Stack>
    </WithGameEditTab>
  )
}

export default Managers
