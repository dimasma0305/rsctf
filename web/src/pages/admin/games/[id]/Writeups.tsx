import {
  Alert,
  Badge,
  Button,
  Group,
  Loader,
  Pagination,
  Paper,
  Progress,
  Select,
  Stack,
  Table,
  Tabs,
  Text,
  Title,
} from '@mantine/core'
import { useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { PDFViewer } from '@Components/admin/PDFViewer'
import { WithGameEditTab } from '@Components/admin/WithGameEditTab'
import { WriteupGradeRow } from '@Components/admin/WriteupGradeRow'
import { downloadBlob } from '@Utils/ApiHelper'
import { rankWriteupTeams, type GradingView } from '@Utils/WriteupGrading'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api from '@Api'
import classes from '@Styles/WriteupGrading.module.css'

const points = (value: number) => value.toLocaleString(undefined, { maximumFractionDigits: 4 })

export default function GameWriteups() {
  const { id } = useParams()
  const gameId = Number(id)
  const { t } = useTranslation()
  const { data, error, isLoading, isValidating, mutate } = api.admin.useWriteupGrading(gameId, OnceSWRConfig)
  const [division, setDivision] = useState('')
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [view, setView] = useState<GradingView>('Overall')
  const [tab, setTab] = useState<string | null>('review')
  const [page, setPage] = useState(1)
  const [downloading, setDownloading] = useState(false)
  const [status, setStatus] = useState('')
  const rows = useMemo(() => rankWriteupTeams(data?.teams ?? [], view, division), [data, view, division])
  const selected = rows.find((r) => String(r.team.participationId) === selectedId) ?? rows[0]
  const divisionOptions = [
    ...new Map(
      (data?.teams ?? [])
        .filter((team) => team.divisionId !== null)
        .map((team) => [String(team.divisionId), team.division ?? String(team.divisionId)])
    ).entries(),
  ].map(([value, label]) => ({ value, label }))
  const pageCount = Math.max(1, Math.ceil(rows.length / 25))
  const currentPage = Math.min(page, pageCount)
  const reviewed = selected?.team.challenges.filter((c) => c.percentage !== null).length ?? 0
  const downloadAll = () =>
    downloadBlob(
      `admin:writeups:${gameId}`,
      () => api.admin.adminDownloadAllWriteups(gameId, { format: 'blob' }),
      setDownloading,
      t
    )

  return (
    <WithGameEditTab
      head={
        <Group gap="xs">
          <Button
            variant="default"
            loading={isValidating}
            onClick={() => {
              setStatus('')
              void mutate()
            }}
          >
            {t('admin.grading.refresh', 'Refresh scores & grades')}
          </Button>
          <Button variant="default" loading={downloading} onClick={downloadAll}>
            {t('admin.button.writeups.download_all')}
          </Button>
        </Group>
      }
    >
      <Stack w="100%" gap="md" data-writeup-grading>
        <Paper withBorder p="md" radius="md">
          <Group justify="space-between" gap="sm">
            <Title order={2} size="h3">
              {t('admin.grading.title', 'Writeup review & private ranking')}
            </Title>
            <Badge variant="outline">{t('admin.grading.private', 'Admin only')}</Badge>
          </Group>
          <Text size="sm" mt="xs">
            {t(
              'admin.grading.explanation',
              'Graded points = earned points × writeup grade. Ungraded challenges retain 100%. Public scores never change.'
            )}
          </Text>
          <Text size="xs" c="dimmed" mt={4}>
            {t(
              'admin.grading.formats',
              'Overall uses the event’s format weights. Jeopardy includes solve bonuses; A&D and KoTH use finalized challenge contributions. Equal projected scores share a rank.'
            )}
          </Text>
          {data && (
            <Text size="xs" c="dimmed" mt={4}>
              {t('admin.grading.snapshot', 'Score snapshot: {{time}}', {
                time: new Date(data.generatedAt).toLocaleString(),
              })}
            </Text>
          )}
        </Paper>
        {error ? (
          <Alert color="red" title={t('admin.grading.failed', 'Writeup grading could not be loaded')}>
            {t(
              'admin.grading.retry',
              'Administrator access is required. Try refreshing; existing grades have not been changed.'
            )}
          </Alert>
        ) : isLoading ? (
          <Group role="status">
            <Loader size="sm" />
            <Text>{t('admin.grading.loading', 'Loading scores and writeups…')}</Text>
          </Group>
        ) : null}
        {data && !data.fullySettled && (
          <Alert color="yellow">
            {t(
              'admin.grading.unsettled',
              'Some rounds are not finalized yet. These results are provisional; refresh after scoring settles.'
            )}
          </Alert>
        )}
        <Text size="sm" role="status" aria-live="polite">
          {status}
        </Text>
        {data && !error && (
          <>
            <Group align="end" grow>
              <Select
                label={t('admin.grading.division', 'Division')}
                value={division}
                allowDeselect={false}
                data={[{ value: '', label: t('game.label.score_table.all_teams') }, ...divisionOptions]}
                onChange={(v) => {
                  setDivision(v ?? '')
                  setPage(1)
                }}
              />
              <Select
                label={t('admin.grading.score_view', 'Score view')}
                value={view}
                allowDeselect={false}
                data={[
                  { value: 'Overall', label: t('admin.grading.overall', 'Overall · 0–100') },
                  { value: 'Jeopardy', label: 'Jeopardy' },
                  { value: 'AttackDefense', label: 'A&D' },
                  { value: 'KingOfTheHill', label: 'KoTH' },
                ]}
                onChange={(v) => {
                  setView((v as GradingView) ?? 'Overall')
                  setPage(1)
                }}
              />
            </Group>
            <Tabs value={tab} onChange={setTab}>
              <Tabs.List>
                <Tabs.Tab value="review">{t('admin.grading.review', 'Review a team')}</Tabs.Tab>
                <Tabs.Tab value="ranking">{t('admin.grading.ranking', 'Projected scoreboard')}</Tabs.Tab>
              </Tabs.List>
              <Tabs.Panel value="review" pt="md">
                <Stack>
                  <Select
                    searchable
                    label={t('admin.grading.team', 'Team to review')}
                    value={selected ? String(selected.team.participationId) : null}
                    nothingFoundMessage={t('admin.grading.no_teams', 'No matching teams')}
                    data={rows.map((r) => ({
                      value: String(r.team.participationId),
                      label: `${r.team.name} · ${r.team.challenges.filter((c) => c.percentage !== null).length}/${r.team.challenges.length}`,
                    }))}
                    onChange={setSelectedId}
                    allowDeselect={false}
                    limit={50}
                  />
                  {selected ? (
                    <>
                      <Paper withBorder p="md" radius="md">
                        <Title order={3} size="h4" style={{ overflowWrap: 'anywhere' }}>
                          {selected.team.name}
                        </Title>
                        <Group justify="space-between" mt="xs">
                          <Text>
                            {t('admin.grading.score_change', '{{original}} → {{score}} points', {
                              original: points(selected.original),
                              score: points(selected.score),
                            })}
                          </Text>
                          <Text fw={700}>
                            {selected.rank
                              ? t('admin.grading.rank', 'Projected rank #{{rank}}', { rank: selected.rank })
                              : t('admin.grading.unranked', 'Unranked')}
                          </Text>
                        </Group>
                        <Text size="sm" c="dimmed" mt="xs">
                          {t('admin.grading.progress', '{{reviewed}} of {{total}} challenges graded', {
                            reviewed,
                            total: selected.team.challenges.length,
                          })}
                        </Text>
                        <Progress
                          value={
                            selected.team.challenges.length ? (reviewed / selected.team.challenges.length) * 100 : 0
                          }
                          aria-label={t('admin.grading.review_progress', 'Review progress')}
                          mt={6}
                        />
                      </Paper>
                      <div className={classes.reviewLayout}>
                        <Stack gap="sm" miw={0}>
                          <Title order={3} size="h4">
                            {t('admin.grading.earned', 'Scored challenges')}
                          </Title>
                          {!selected.team.challenges.length && (
                            <Text c="dimmed">{t('admin.grading.no_scores', 'No scored challenges to grade yet.')}</Text>
                          )}
                          {selected.team.challenges.map((challenge) => (
                            <WriteupGradeRow
                              key={`${selected.team.participationId}:${challenge.challengeId}`}
                              gameId={gameId}
                              participationId={selected.team.participationId}
                              challenge={challenge}
                              onSaved={async (result) => {
                                setSelectedId(String(result.participationId))
                                await mutate(
                                  (current) =>
                                    current
                                      ? {
                                          ...current,
                                          teams: current.teams.map((team) =>
                                            team.participationId !== result.participationId
                                              ? team
                                              : {
                                                  ...team,
                                                  challenges: team.challenges.map((cell) =>
                                                    cell.challengeId !== result.challengeId
                                                      ? cell
                                                      : {
                                                          ...cell,
                                                          percentage: result.percentage,
                                                          revision: result.revision,
                                                        }
                                                  ),
                                                }
                                          ),
                                        }
                                      : current,
                                  { revalidate: false }
                                )
                                setStatus(
                                  t('admin.grading.saved', 'Grade saved. The private ranking has been updated.')
                                )
                              }}
                            />
                          ))}
                        </Stack>
                        <Stack gap="sm" miw={0}>
                          <Title order={3} size="h4">
                            {t('admin.grading.document', 'Submitted writeup')}
                          </Title>
                          {selected.team.writeupUrl ? (
                            <>
                              <Button
                                component="a"
                                href={selected.team.writeupUrl}
                                target="_blank"
                                rel="noreferrer"
                                variant="default"
                              >
                                {t('admin.grading.open', 'Open / download writeup')}
                              </Button>
                              <PDFViewer key={selected.team.writeupUrl} url={selected.team.writeupUrl} height="65vh" />
                            </>
                          ) : (
                            <Paper withBorder p="md">
                              <Text>
                                {t(
                                  'admin.grading.missing',
                                  'No writeup submitted. You can still grade a challenge 0% if its writeup is missing.'
                                )}
                              </Text>
                            </Paper>
                          )}
                        </Stack>
                      </div>
                    </>
                  ) : (
                    <Text>{t('admin.grading.no_teams', 'No matching teams')}</Text>
                  )}
                </Stack>
              </Tabs.Panel>
              <Tabs.Panel value="ranking" pt="md">
                <Stack>
                  <Title order={3} size="h4">
                    {t('admin.grading.ranking', 'Projected scoreboard')}
                  </Title>
                  <div
                    className={classes.tableScroll}
                    role="region"
                    aria-label={t('admin.grading.ranking', 'Projected scoreboard')}
                    tabIndex={0}
                  >
                    <Table striped highlightOnHover>
                      <Table.Caption>
                        {t(
                          'admin.grading.private_caption',
                          'Private projection from saved writeup grades. Competition scores are unchanged.'
                        )}
                      </Table.Caption>
                      <Table.Thead>
                        <Table.Tr>
                          {[
                            t('admin.grading.rank_label', 'Rank'),
                            t('admin.grading.team_label', 'Team'),
                            t('admin.grading.original', 'Original'),
                            t('admin.grading.projected', 'Graded'),
                            t('admin.grading.reviewed', 'Reviewed'),
                          ].map((label) => (
                            <Table.Th scope="col" key={label}>
                              {label}
                            </Table.Th>
                          ))}
                        </Table.Tr>
                      </Table.Thead>
                      <Table.Tbody>
                        {rows.slice((currentPage - 1) * 25, currentPage * 25).map((row) => (
                          <Table.Tr key={row.team.participationId}>
                            <Table.Td>
                              <Text fw={700}>{row.rank ? `#${row.rank}` : '—'}</Text>
                            </Table.Td>
                            <Table.Th scope="row">
                              <Button
                                variant="subtle"
                                size="compact-sm"
                                className={classes.teamButton}
                                onClick={() => {
                                  setSelectedId(String(row.team.participationId))
                                  setTab('review')
                                }}
                              >
                                {row.team.name}
                              </Button>
                            </Table.Th>
                            <Table.Td>{points(row.original)}</Table.Td>
                            <Table.Td>
                              <Text fw={700}>{points(row.score)}</Text>
                            </Table.Td>
                            <Table.Td>
                              {row.team.challenges.filter((c) => c.percentage !== null).length}/
                              {row.team.challenges.length}
                            </Table.Td>
                          </Table.Tr>
                        ))}
                      </Table.Tbody>
                    </Table>
                  </div>
                  {pageCount > 1 && (
                    <Pagination
                      total={pageCount}
                      value={currentPage}
                      onChange={setPage}
                      siblings={0}
                      boundaries={1}
                      aria-label={t('admin.grading.pages', 'Ranking pages')}
                    />
                  )}
                </Stack>
              </Tabs.Panel>
            </Tabs>
          </>
        )}
      </Stack>
    </WithGameEditTab>
  )
}
