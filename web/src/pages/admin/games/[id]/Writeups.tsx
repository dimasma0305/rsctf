import { Button, Center, Flex, Pagination, ScrollArea, Select, Stack, Text, Title } from '@mantine/core'
import { mdiFolderDownloadOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useState, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { useParams } from 'react-router'
import { PDFViewer } from '@Components/admin/PDFViewer'
import { TeamWriteupCard } from '@Components/admin/TeamWriteupCard'
import { WithGameEditTab } from '@Components/admin/WithGameEditTab'
import { downloadBlob } from '@Utils/ApiHelper'
import { useIsMobile } from '@Utils/ThemeOverride'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, { WriteupInfo } from '@Api'

const WRITEUPS_PER_PAGE = 50

const GameWriteups: FC = () => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')
  const [selected, setSelected] = useState<WriteupInfo>()
  const [selectedDivision, setSelectedDivision] = useState<string>('')
  const [page, setPage] = useState(1)
  const [downloading, setDownloading] = useState(false)

  const query = useMemo(
    () => ({
      count: WRITEUPS_PER_PAGE,
      skip: (page - 1) * WRITEUPS_PER_PAGE,
      divisionId: selectedDivision ? parseInt(selectedDivision) : undefined,
    }),
    [page, selectedDivision]
  )
  const { data } = api.admin.useAdminWriteups(numId, query, OnceSWRConfig)
  const { t } = useTranslation()
  const isCompact = useIsMobile(900)
  const totalPages = Math.max(1, Math.ceil((data?.total ?? 0) / WRITEUPS_PER_PAGE))

  const writeups = useMemo(() => data?.writeups ?? [], [data?.writeups])

  const { divisions, divisionOptions } = useMemo(() => {
    const rawDivisions = data?.divisions ?? {}
    const divisions = Object.fromEntries(Object.entries(rawDivisions).map(([k, v]) => [parseInt(k), v]))
    const divisionOptions = [
      { value: '', label: t('game.label.score_table.all_teams') },
      ...Object.entries(divisions).map(([id, name]) => ({ value: id.toString(), label: name })),
    ]
    return { divisions, divisionOptions }
  }, [data?.divisions, t])

  const filteredWriteups = writeups

  useEffect(() => {
    if (filteredWriteups?.length && (!selected || !filteredWriteups.some((w) => w.id === selected.id))) {
      setSelected(filteredWriteups[0])
    }
  }, [filteredWriteups, selected])

  useEffect(() => {
    if (page > totalPages) setPage(totalPages)
  }, [page, totalPages])

  const downloadAll = () =>
    downloadBlob(
      `admin:writeups:${numId}`,
      () => api.admin.adminDownloadAllWriteups(numId, { format: 'blob' }),
      setDownloading,
      t
    )

  return (
    <WithGameEditTab
      headProps={{ justify: 'space-between' }}
      contentPos="right"
      head={
        <Button
          fullWidth
          w={isCompact ? '100%' : '15rem'}
          leftSection={<Icon path={mdiFolderDownloadOutline} size={1} />}
          disabled={downloading}
          loading={downloading}
          onClick={downloadAll}
        >
          {t('admin.button.writeups.download_all')}
        </Button>
      }
    >
      <Flex direction={isCompact ? 'column' : 'row'} gap="md" align="flex-start" justify="space-between" w="100%">
        {!filteredWriteups?.length || !selected ? (
          <Center w="100%" mih={isCompact ? '40dvh' : 'calc(100vh - 180px)'}>
            <Stack gap={0}>
              <Title order={2}>{t('admin.content.games.writeups.empty.title')}</Title>
              <Text>{t('admin.content.games.writeups.empty.description')}</Text>
            </Stack>
          </Center>
        ) : (
          <Stack pos="relative" mt={isCompact ? 0 : '-3rem'} w={isCompact ? '100%' : 'calc(100% - 120px)'}>
            <PDFViewer url={selected?.url} height={isCompact ? '70dvh' : 'calc(100vh - 110px)'} />
          </Stack>
        )}
        <Stack
          gap="sm"
          w={isCompact ? '100%' : '15rem'}
          miw={isCompact ? 0 : '15rem'}
          maw={isCompact ? 'none' : '15rem'}
          h={isCompact ? 'auto' : 'calc(100vh - 110px - 3rem)'}
          style={{ order: isCompact ? -1 : 0 }}
        >
          <Select
            label={t('admin.label.games.writeups.division', 'Division')}
            data={divisionOptions}
            value={selectedDivision}
            onChange={(value) => {
              setSelectedDivision(value ?? '')
              setPage(1)
            }}
          />
          <ScrollArea type="auto" h={isCompact ? '22rem' : undefined} style={{ flex: 1 }}>
            <Stack gap="sm">
              {filteredWriteups?.map((writeup) => (
                <TeamWriteupCard
                  key={writeup.id}
                  writeup={writeup}
                  selected={selected?.id === writeup.id}
                  onClick={() => setSelected(writeup)}
                  divisionName={writeup.divisionId ? divisions[writeup.divisionId] : undefined}
                />
              ))}
            </Stack>
          </ScrollArea>
          {totalPages > 1 && (
            <Pagination
              total={totalPages}
              value={page}
              onChange={setPage}
              size="sm"
              siblings={0}
              boundaries={0}
              withEdges
              aria-label={t('admin.label.games.writeups.page', 'Writeup pages')}
            />
          )}
        </Stack>
      </Flex>
    </WithGameEditTab>
  )
}

export default GameWriteups
