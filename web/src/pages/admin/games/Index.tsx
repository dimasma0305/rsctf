import {
  ActionIcon,
  Avatar,
  Badge,
  Box,
  Button,
  Card,
  Code,
  Divider,
  FileButton,
  Group,
  Paper,
  Progress,
  ScrollArea,
  Stack,
  Switch,
  Table,
  Text,
  UnstyledButton,
  alpha,
  useMantineTheme,
} from '@mantine/core'
import {
  mdiArrowLeftBold,
  mdiArrowRightBold,
  mdiChevronTripleRight,
  mdiContentDuplicate,
  mdiOpenInNew,
  mdiPencilOutline,
  mdiPlus,
  mdiUpload,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { Link, useNavigate } from 'react-router'
import { GameColorMap } from '@Components/GameCard'
import { AdminPage } from '@Components/admin/AdminPage'
import { CloneGameModal } from '@Components/admin/CloneGameModal'
import { GameCreateModal } from '@Components/admin/GameCreateModal'
import { showErrorMsg } from '@Utils/Shared'
import { useArrayResponse } from '@Hooks/useArrayResponse'
import { getGameStatus } from '@Hooks/useGame'
import { useUser } from '@Hooks/useUser'
import api, { GameInfoModel, Role } from '@Api'
import misc from '@Styles/Misc.module.css'
import tableClasses from '@Styles/Table.module.css'
import uploadClasses from '@Styles/Upload.module.css'
import mobileClasses from '../AdminMobileList.module.css'

const ITEM_COUNT_PER_PAGE = 30

const Games: FC = () => {
  const [page, setPage] = useState(1)
  const [createOpened, setCreateOpened] = useState(false)
  const [cloneTarget, setCloneTarget] = useState<GameInfoModel | null>(null)
  const [disabled, setDisabled] = useState(false)
  const [progress, setProgress] = useState(0)
  const { data: games, total, setData: setGames, updateData: updateGames } = useArrayResponse<GameInfoModel>()
  const [current, setCurrent] = useState(0)
  const { user } = useUser()

  const navigate = useNavigate()
  const { t } = useTranslation()
  const theme = useMantineTheme()

  const onToggleHidden = async (game: GameInfoModel) => {
    if (!game.id) return
    setDisabled(true)

    try {
      await api.edit.editUpdateGame(game.id, {
        ...game,
        hidden: !game.hidden,
      })
      if (games) {
        updateGames(
          games.map((g) => {
            if (g.id === game.id) {
              return { ...g, hidden: !g.hidden }
            }
            return g
          })
        )
      }
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const onImportGame = async (file: File | null) => {
    if (!file) return

    setProgress(0)
    setDisabled(true)

    try {
      const res = await api.edit.editImportGame(
        { file },
        {
          onUploadProgress: (e) => {
            setProgress((e.loaded / (e.total ?? 1)) * 100)
          },
        }
      )

      setProgress(0)
      setDisabled(false)

      if (res.data) {
        // Refresh the games list
        const gamesRes = await api.edit.editGetGames({
          count: ITEM_COUNT_PER_PAGE,
          skip: (page - 1) * ITEM_COUNT_PER_PAGE,
        })
        setGames(gamesRes.data)
        setCurrent((page - 1) * ITEM_COUNT_PER_PAGE + gamesRes.data.length)

        // Navigate to the imported game
        navigate(`/admin/games/${res.data}/info`)
      }
    } catch (err) {
      showErrorMsg(err, t)
      setProgress(0)
      setDisabled(false)
    }
  }

  useEffect(() => {
    const fetchData = async () => {
      try {
        const res = await api.edit.editGetGames({
          count: ITEM_COUNT_PER_PAGE,
          skip: (page - 1) * ITEM_COUNT_PER_PAGE,
        })
        setGames(res.data)
        setCurrent((page - 1) * ITEM_COUNT_PER_PAGE + res.data.length)
      } catch (e) {
        showErrorMsg(e, t)
      }
    }

    fetchData()
  }, [page])

  return (
    <AdminPage
      isLoading={!games}
      headProps={{ justify: 'space-between' }}
      head={
        <>
          {user?.role === Role.Admin && (
            <Group grow gap="sm" wrap="nowrap" w={{ base: '100%', sm: 'auto' }}>
              <Button h={44} leftSection={<Icon path={mdiPlus} size={1} />} onClick={() => setCreateOpened(true)}>
                {t('admin.button.games.new')}
              </Button>
              <FileButton onChange={onImportGame} accept="application/zip">
                {(props) => (
                  <Button
                    {...props}
                    leftSection={<Icon path={mdiUpload} size={1} />}
                    className={uploadClasses.button}
                    disabled={disabled}
                    color={progress !== 0 ? 'cyan' : theme.primaryColor}
                    variant="outline"
                    h={44}
                  >
                    <div className={uploadClasses.label}>
                      {progress !== 0 ? t('admin.notification.games.import.importing') : t('admin.button.games.import')}
                    </div>
                    {progress !== 0 && (
                      <Progress
                        value={progress}
                        className={uploadClasses.progress}
                        color={alpha(theme.colors[theme.primaryColor][2], 0.35)}
                        radius="sm"
                      />
                    )}
                  </Button>
                )}
              </FileButton>
            </Group>
          )}
          <Group w={{ base: '100%', sm: 'auto' }} justify="space-between" gap="sm">
            <Text fw="bold" size="sm">
              <Trans
                i18nKey="admin.content.games.stats"
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
      <Paper shadow="md" p="md" w="100%">
        <Box visibleFrom="sm">
          <ScrollArea offsetScrollbars h="calc(100vh - 190px)">
            <Table className={tableClasses.table}>
              <Table.Caption className="app-sr-only">{t('admin.content.games.table_caption', 'Games')}</Table.Caption>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th scope="col" miw="1.8rem">
                    {t('admin.label.games.hide')}
                  </Table.Th>
                  <Table.Th scope="col">{t('common.label.game')}</Table.Th>
                  <Table.Th scope="col">{t('common.label.time')}</Table.Th>
                  <Table.Th scope="col">{t('admin.label.games.summary')}</Table.Th>
                  <Table.Th scope="col" aria-label={t('common.label.action', 'Actions')} />
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {games &&
                  games.map((game) => {
                    const { startTime, endTime, status } = getGameStatus(game)
                    const color = GameColorMap.get(status)

                    return (
                      <Table.Tr key={game.id}>
                        <Table.Td>
                          <Switch
                            disabled={disabled}
                            checked={game.hidden}
                            aria-label={t('admin.label.games.toggle_hidden', 'Toggle visibility for {{name}}', {
                              name: game.title,
                            })}
                            onChange={() => onToggleHidden(game)}
                          />
                        </Table.Td>
                        <Table.Td>
                          <Group wrap="nowrap" justify="space-between">
                            <UnstyledButton component={Link} to={`/games/${game.id}`} className={misc.cPointer}>
                              <Group wrap="nowrap" justify="left">
                                <Avatar imageProps={{ loading: 'lazy' }} alt="avatar" src={game.poster} radius={0}>
                                  {game.title?.slice(0, 1)}
                                </Avatar>
                                <Text fw="bold" lineClamp={1} maw="calc(20vw)">
                                  {game.title}
                                </Text>
                              </Group>
                            </UnstyledButton>
                            <Badge color={color}>{status}</Badge>
                          </Group>
                        </Table.Td>
                        <Table.Td>
                          <Group wrap="nowrap" gap="xs">
                            <Badge size="sm" color={color} variant="dot">
                              {dayjs(startTime).format('YYYY-MM-DD HH:mm')}
                            </Badge>
                            <Icon path={mdiChevronTripleRight} size={1} />
                            <Badge size="sm" color={color} variant="dot">
                              {dayjs(endTime).format('YYYY-MM-DD HH:mm')}
                            </Badge>
                          </Group>
                        </Table.Td>
                        <Table.Td>
                          <Text size="sm" truncate maw="20rem">
                            {game.summary}
                          </Text>
                        </Table.Td>
                        <Table.Td>
                          <Group justify="right" gap="xs">
                            {user?.role === Role.Admin && (
                              <ActionIcon
                                variant="subtle"
                                color="cyan"
                                aria-label={t('admin.button.games.clone', 'Clone {{name}}', { name: game.title })}
                                onClick={() => setCloneTarget(game)}
                              >
                                <Icon path={mdiContentDuplicate} size={1} />
                              </ActionIcon>
                            )}
                            <ActionIcon
                              component={Link}
                              to={`/admin/games/${game.id}/info`}
                              aria-label={t('admin.button.games.edit', 'Edit {{name}}', { name: game.title })}
                            >
                              <Icon path={mdiPencilOutline} size={1} />
                            </ActionIcon>
                          </Group>
                        </Table.Td>
                      </Table.Tr>
                    )
                  })}
              </Table.Tbody>
            </Table>
          </ScrollArea>
        </Box>
        <Stack hiddenFrom="sm" gap="sm" className={mobileClasses.mobileList}>
          {games?.map((game) => {
            const { startTime, endTime, status } = getGameStatus(game)
            const color = GameColorMap.get(status)
            const gameHeadingId = `mobile-game-${game.id}`
            const gameTitle = game.title || t('common.label.game')

            return (
              <Card
                component="article"
                key={game.id}
                withBorder
                radius="lg"
                p="md"
                className={mobileClasses.card}
                aria-labelledby={gameHeadingId}
              >
                <Stack gap="md">
                  <Group wrap="nowrap" align="center" gap="sm">
                    <Avatar imageProps={{ loading: 'lazy' }} alt="" src={game.poster} radius="md" size={52}>
                      {gameTitle.slice(0, 1)}
                    </Avatar>
                    <Stack gap={2} className={mobileClasses.identity}>
                      <UnstyledButton
                        component={Link}
                        to={`/games/${game.id}`}
                        className={mobileClasses.publicLink}
                        aria-label={t('admin.button.games.open_public', 'Open public page for {{name}}', {
                          name: gameTitle,
                        })}
                      >
                        <Group wrap="nowrap" gap={6} w="100%">
                          <Text
                            component="h2"
                            id={gameHeadingId}
                            size="sm"
                            fw={750}
                            className={mobileClasses.recordTitle}
                          >
                            {gameTitle}
                          </Text>
                          <Icon path={mdiOpenInNew} size={0.7} aria-hidden="true" />
                        </Group>
                      </UnstyledButton>
                      <Group gap={6} wrap="nowrap">
                        <Text className={mobileClasses.detailLabel}>{t('common.label.status')}</Text>
                        <Badge size="sm" variant="light" color={color}>
                          {status}
                        </Badge>
                      </Group>
                    </Stack>
                  </Group>

                  <Group className={mobileClasses.stateRow} justify="space-between" wrap="nowrap">
                    <Stack gap={1}>
                      <Text className={mobileClasses.detailLabel}>
                        {t('admin.label.games.visibility', 'Visibility')}
                      </Text>
                      <Text size="sm" fw={650}>
                        {game.hidden
                          ? t('admin.label.games.hidden_state', 'Hidden from players')
                          : t('admin.label.games.visible_state', 'Visible to players')}
                      </Text>
                    </Stack>
                    <Switch
                      h={44}
                      size="md"
                      disabled={disabled}
                      checked={game.hidden}
                      aria-label={t('admin.label.games.toggle_hidden', 'Toggle visibility for {{name}}', {
                        name: gameTitle,
                      })}
                      onChange={() => onToggleHidden(game)}
                    />
                  </Group>

                  <Box component="dl" className={mobileClasses.details}>
                    <Box component="div" className={mobileClasses.detail}>
                      <Text component="dt" className={mobileClasses.detailLabel}>
                        {t('admin.label.games.starts', 'Starts')}
                      </Text>
                      <Text component="dd" size="sm" ff="monospace" className={mobileClasses.detailValue}>
                        {dayjs(startTime).format('YYYY-MM-DD HH:mm')}
                      </Text>
                    </Box>
                    <Box component="div" className={mobileClasses.detail}>
                      <Text component="dt" className={mobileClasses.detailLabel}>
                        {t('admin.label.games.ends', 'Ends')}
                      </Text>
                      <Text component="dd" size="sm" ff="monospace" className={mobileClasses.detailValue}>
                        {dayjs(endTime).format('YYYY-MM-DD HH:mm')}
                      </Text>
                    </Box>
                    <Box component="div" className={[mobileClasses.detail, mobileClasses.detailWide].join(' ')}>
                      <Text component="dt" className={mobileClasses.detailLabel}>
                        {t('admin.label.games.summary')}
                      </Text>
                      <Text component="dd" size="sm" className={mobileClasses.detailValue}>
                        {game.summary || '—'}
                      </Text>
                    </Box>
                  </Box>

                  <Divider />
                  <Box
                    component="section"
                    aria-label={t('common.label.action', 'Actions')}
                    className={mobileClasses.gameActions}
                  >
                    {user?.role === Role.Admin && (
                      <Button
                        h={44}
                        variant="light"
                        color="cyan"
                        leftSection={<Icon path={mdiContentDuplicate} size={0.85} />}
                        aria-label={t('admin.button.games.clone', 'Clone {{name}}', { name: gameTitle })}
                        onClick={() => setCloneTarget(game)}
                      >
                        {t('admin.button.games.clone_short', 'Clone')}
                      </Button>
                    )}
                    <Button
                      component={Link}
                      to={`/admin/games/${game.id}/info`}
                      h={44}
                      leftSection={<Icon path={mdiPencilOutline} size={0.85} />}
                      aria-label={t('admin.button.games.edit', 'Edit {{name}}', { name: gameTitle })}
                    >
                      {t('admin.button.games.edit_short', 'Edit game')}
                    </Button>
                  </Box>
                </Stack>
              </Card>
            )
          })}
        </Stack>
      </Paper>
      <GameCreateModal
        opened={createOpened}
        onClose={() => setCreateOpened(false)}
        onAddGame={(game) => updateGames([...(games ?? []), game])}
      />
      <CloneGameModal game={cloneTarget} opened={!!cloneTarget} onClose={() => setCloneTarget(null)} />
    </AdminPage>
  )
}

export default Games
