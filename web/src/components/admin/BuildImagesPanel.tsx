import {
  ActionIcon,
  Badge,
  Button,
  Center,
  Code,
  CopyButton,
  Group,
  Loader,
  Paper,
  ScrollArea,
  Stack,
  Table,
  Text,
  Title,
  Tooltip,
} from '@mantine/core'
import { useModals } from '@mantine/modals'
import { showNotification } from '@mantine/notifications'
import { mdiBroom, mdiCheck, mdiContentCopy, mdiDatabaseOutline, mdiDeleteOutline, mdiRefresh } from '@mdi/js'
import { Icon } from '@mdi/react'
import cx from 'clsx'
import dayjs from 'dayjs'
import relativeTime from 'dayjs/plugin/relativeTime'
import { FC, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { showErrorMsg } from '@Utils/Shared'
import api, { BuildImageModel } from '@Api'
import tableClasses from '@Styles/Table.module.css'

dayjs.extend(relativeTime)

// Binary (1024) units — matches what `docker images` reports.
const formatBytes = (n: number): string => {
  if (!n) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)))
  const v = n / 1024 ** i
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`
}

// The deterministic content-hash tag; show repo + short tag so rows stay readable.
const shortTag = (tag: string): string => {
  const colon = tag.lastIndexOf(':')
  if (colon < 0) return tag
  return `${tag.slice(0, colon)}:${tag.slice(colon + 1, colon + 13)}`
}

/**
 * Lists the rsctf/* images actually present on the Docker daemon (size, age,
 * which challenges still reference them) and lets the operator delete them one by one
 * — the per-image counterpart to the bulk "Prune images" button.
 */
export const BuildImagesPanel: FC = () => {
  const { t } = useTranslation()
  const modals = useModals()
  const [busy, setBusy] = useState(false)

  // Poll every 30s — each refresh hits the docker daemon (heavier than a DB read), and the
  // manual refresh button covers the "I just deleted something" case.
  const {
    data: images,
    mutate,
    isLoading,
  } = api.admin.useAdminListBuildImages({
    refreshInterval: 30000,
  })
  const { data: storage, mutate: mutateStorage } = api.admin.useAdminBuildStorageStatus({
    refreshInterval: 30000,
  })

  const totalBytes = useMemo(() => (images ?? []).reduce((sum, img) => sum + img.sizeBytes, 0), [images])

  const runStorageCleanup = async () => {
    setBusy(true)
    try {
      const response = await api.admin.adminCleanupBuildStorage()
      const result = response.data
      showNotification({
        color: result.pressureMode ? 'orange' : 'teal',
        message: t('admin.content.builds.images.cleanup_result', {
          defaultValue: 'Removed {{count}} image(s); reclaimed {{cache}} of build cache.',
          count: result.imagesRemoved,
          cache: formatBytes(result.cacheBytesReclaimed + result.danglingBytesReclaimed),
        }),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      await Promise.all([mutate(), mutateStorage()])
    } catch (e: unknown) {
      showErrorMsg(e, t)
    } finally {
      setBusy(false)
    }
  }

  // Delete every rsctf/* tag of one image (usually one, sometimes a registry mirror
  // too). force=true overrides the daemon's "in use by a container" refusal (409).
  const runDelete = async (img: BuildImageModel, force: boolean) => {
    setBusy(true)
    try {
      let removed = 0
      for (const tag of img.tags) {
        const resp = await api.admin.adminDeleteBuildImage({ tag, force })
        removed += resp.data.removed
      }
      showNotification({
        color: 'teal',
        message: t('admin.content.builds.images.deleted', 'Image deleted'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e: unknown) {
      // 409 → the image backs a running container. Offer the force escalation.
      const status = (e as { response?: { status?: number } })?.response?.status
      if (status === 409 && !force) {
        modals.openConfirmModal({
          title: t('admin.content.builds.images.in_use_title', 'Image in use'),
          children: (
            <Text size="sm">
              {t(
                'admin.content.builds.images.in_use_body',
                'This image is in use by a running container. Force-delete it anyway? The container may break until rebuilt.'
              )}
            </Text>
          ),
          confirmProps: { color: 'red' },
          onConfirm: () => runDelete(img, true),
        })
      } else {
        showErrorMsg(e, t)
      }
    } finally {
      // Always refresh so the list reflects reality even after a partial delete or a
      // dismissed force-escalation (a multi-tag image may have lost one tag).
      mutate()
      setBusy(false)
    }
  }

  const onDelete = (img: BuildImageModel) => {
    modals.openConfirmModal({
      title: t('admin.content.builds.images.delete_title', 'Delete image'),
      children: (
        <Stack gap={6}>
          <Code style={{ wordBreak: 'break-all' }}>{img.tags.join('\n')}</Code>
          {img.referenced ? (
            <Text size="sm" c="red">
              {t('admin.content.builds.images.delete_referenced', {
                defaultValue:
                  'Still referenced by: {{titles}}. Deleting it will break the next launch/check until rebuilt.',
                titles: img.referencedBy.join(', '),
              })}
            </Text>
          ) : (
            <Text size="sm" c="dimmed">
              {t('admin.content.builds.images.delete_orphan', 'No challenge references this image — safe to remove.')}
            </Text>
          )}
        </Stack>
      ),
      confirmProps: { color: 'red' },
      onConfirm: () => runDelete(img, false),
    })
  }

  return (
    <Stack gap={6}>
      <Group justify="space-between" align="flex-end" wrap="wrap">
        <Group gap="xs">
          <Icon path={mdiDatabaseOutline} size={0.9} />
          <Title order={5}>{t('admin.content.builds.images.title', 'Images on disk')}</Title>
          {images && (
            <Badge variant="light" color="gray" ff="monospace">
              {t('admin.content.builds.images.summary', {
                defaultValue: '{{count}} image · {{size}}',
                defaultValue_plural: '{{count}} images · {{size}}',
                count: images.length,
                size: formatBytes(totalBytes),
              })}
            </Badge>
          )}
        </Group>
        <Group gap="xs">
          <Button
            size="xs"
            variant="light"
            leftSection={<Icon path={mdiBroom} size={0.75} />}
            loading={busy}
            onClick={runStorageCleanup}
          >
            {t('admin.content.builds.images.clean_now', 'Clean storage now')}
          </Button>
          <Tooltip label={t('admin.content.builds.images.refresh', 'Refresh')}>
            <ActionIcon
              variant="subtle"
              aria-label={t('admin.content.builds.images.refresh', 'Refresh')}
              onClick={() => Promise.all([mutate(), mutateStorage()])}
              disabled={busy}
            >
              <Icon path={mdiRefresh} size={0.9} />
            </ActionIcon>
          </Tooltip>
        </Group>
      </Group>

      {storage && (
        <Paper p="xs" withBorder>
          <Group gap="xl" wrap="wrap">
            <Stack gap={0}>
              <Text size="xs" c="dimmed">
                {t('admin.content.builds.images.disk_free', 'Filesystem free')}
              </Text>
              <Text size="sm" fw={600} c={storage.lowStorage ? 'red' : undefined}>
                {formatBytes(storage.filesystemAvailableBytes)} / {formatBytes(storage.filesystemTotalBytes)}
              </Text>
            </Stack>
            <Stack gap={0}>
              <Text size="xs" c="dimmed">
                {t('admin.content.builds.images.build_cache', 'Docker build cache')}
              </Text>
              <Text size="sm" fw={600}>
                {formatBytes(storage.buildCacheBytes)} ({formatBytes(storage.reclaimableBuildCacheBytes)}{' '}
                {t('admin.content.builds.images.reclaimable', 'reclaimable')})
              </Text>
            </Stack>
            {storage.minimumFreeBytes > 0 && (
              <Badge color={storage.lowStorage ? 'red' : 'teal'} variant="light">
                {storage.lowStorage
                  ? t('admin.content.builds.images.pressure', 'pressure cleanup active')
                  : t('admin.content.builds.images.floor_ok', 'free-space floor satisfied')}
              </Badge>
            )}
          </Group>
        </Paper>
      )}

      {isLoading && !images ? (
        <Center py="sm">
          <Loader size="xs" />
        </Center>
      ) : !images || images.length === 0 ? (
        <Text size="sm" c="dimmed">
          {t('admin.content.builds.images.empty', 'No build images on disk.')}
        </Text>
      ) : (
        <Paper p="xs" withBorder>
          <ScrollArea>
            <Table
              withTableBorder
              striped
              highlightOnHover
              w="100%"
              miw={900}
              className={cx(tableClasses.table, tableClasses.fixed)}
            >
              <Table.Caption>{t('admin.content.builds.images.title', 'Build images')}</Table.Caption>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th scope="col" w="100%">
                    {t('admin.content.builds.images.column.image', 'Image')}
                  </Table.Th>
                  <Table.Th scope="col" w="6rem">
                    {t('admin.content.builds.images.column.kind', 'Kind')}
                  </Table.Th>
                  <Table.Th scope="col" w="6rem">
                    {t('admin.content.builds.images.column.size', 'Size')}
                  </Table.Th>
                  <Table.Th scope="col" w="8rem">
                    {t('admin.content.builds.images.column.created', 'Created')}
                  </Table.Th>
                  <Table.Th scope="col" w="9rem">
                    {t('admin.content.builds.images.column.usage', 'Usage')}
                  </Table.Th>
                  <Table.Th scope="col" w="10rem">
                    {t('admin.content.builds.images.column.retention', 'Retention')}
                  </Table.Th>
                  <Table.Th scope="col" w="4rem" aria-label={t('common.label.action', 'Actions')} />
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {images.map((img) => (
                  <Table.Tr key={img.id}>
                    <Table.Td>
                      <Group gap={4} wrap="nowrap" miw={0}>
                        <Tooltip label={img.tags.join('\n')} multiline w={420}>
                          <Code
                            style={{
                              display: 'block',
                              flex: 1,
                              minWidth: 0,
                              overflow: 'hidden',
                              textOverflow: 'ellipsis',
                              whiteSpace: 'nowrap',
                            }}
                          >
                            {shortTag(img.tags[0])}
                            {img.tags.length > 1 ? ` +${img.tags.length - 1}` : ''}
                          </Code>
                        </Tooltip>
                        <CopyButton value={img.tags[0]} timeout={1500}>
                          {({ copied, copy }) => (
                            <Tooltip
                              label={
                                copied
                                  ? t('admin.button.builds.copied', 'Copied')
                                  : t('admin.button.builds.copy', 'Copy')
                              }
                            >
                              <ActionIcon
                                variant="subtle"
                                size="sm"
                                color={copied ? 'teal' : 'gray'}
                                aria-label={
                                  copied
                                    ? t('admin.button.builds.copied', 'Copied')
                                    : t('admin.button.builds.copy', 'Copy')
                                }
                                onClick={copy}
                              >
                                <Icon path={copied ? mdiCheck : mdiContentCopy} size={0.7} />
                              </ActionIcon>
                            </Tooltip>
                          )}
                        </CopyButton>
                      </Group>
                    </Table.Td>
                    <Table.Td>
                      <Badge size="xs" variant="light" color={img.isChecker ? 'grape' : 'gray'}>
                        {img.isChecker
                          ? t('admin.content.builds.kind.checker', 'checker')
                          : t('admin.content.builds.kind.service', 'service')}
                      </Badge>
                    </Table.Td>
                    <Table.Td>
                      <Text size="sm" ff="monospace">
                        {formatBytes(img.sizeBytes)}
                      </Text>
                    </Table.Td>
                    <Table.Td>
                      {img.createdUtc ? (
                        <Tooltip label={dayjs(img.createdUtc).format('YYYY-MM-DD HH:mm')}>
                          <Text size="sm">{dayjs(img.createdUtc).fromNow()}</Text>
                        </Tooltip>
                      ) : (
                        <Text size="xs" c="dimmed">
                          —
                        </Text>
                      )}
                    </Table.Td>
                    <Table.Td>
                      {img.inUse ? (
                        <Badge size="sm" color="teal" variant="light">
                          {t('admin.content.builds.images.container_in_use', 'container active')}
                        </Badge>
                      ) : img.referenced ? (
                        <Tooltip label={img.referencedBy.join(', ')} multiline w={300}>
                          <Badge size="sm" color="blue" variant="light" style={{ cursor: 'default' }}>
                            {t('admin.content.builds.images.in_use', {
                              defaultValue: 'in use ({{count}})',
                              count: img.referencedBy.length,
                            })}
                          </Badge>
                        </Tooltip>
                      ) : (
                        <Badge size="sm" color="gray" variant="outline">
                          {t('admin.content.builds.images.orphan', 'orphan')}
                        </Badge>
                      )}
                    </Table.Td>
                    <Table.Td>
                      <Tooltip
                        label={
                          img.lastUsedUtc
                            ? `${t('admin.content.builds.images.last_used', 'Last used')}: ${dayjs(img.lastUsedUtc).format('YYYY-MM-DD HH:mm')}`
                            : t('admin.content.builds.images.never_started', 'Built but never started')
                        }
                      >
                        <Badge
                          size="sm"
                          variant="outline"
                          color={dayjs(img.retentionExpiresUtc).isBefore(dayjs()) ? 'orange' : 'gray'}
                        >
                          {dayjs(img.retentionExpiresUtc).isBefore(dayjs())
                            ? t('admin.content.builds.images.expired', 'expired')
                            : t('admin.content.builds.images.expires', {
                                defaultValue: 'expires {{time}}',
                                time: dayjs(img.retentionExpiresUtc).fromNow(),
                              })}
                        </Badge>
                      </Tooltip>
                    </Table.Td>
                    <Table.Td>
                      <Group justify="flex-end">
                        <Tooltip label={t('admin.button.builds.delete', 'Delete')}>
                          <ActionIcon
                            variant="subtle"
                            color="red"
                            disabled={busy}
                            aria-label={t('admin.button.builds.delete', 'Delete')}
                            onClick={() => onDelete(img)}
                          >
                            <Icon path={mdiDeleteOutline} size={0.9} />
                          </ActionIcon>
                        </Tooltip>
                      </Group>
                    </Table.Td>
                  </Table.Tr>
                ))}
              </Table.Tbody>
            </Table>
          </ScrollArea>
        </Paper>
      )}
    </Stack>
  )
}

export default BuildImagesPanel
