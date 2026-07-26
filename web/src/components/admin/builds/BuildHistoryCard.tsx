import {
  ActionIcon,
  Anchor,
  Badge,
  Button,
  Checkbox,
  Code,
  CopyButton,
  Group,
  Paper,
  SimpleGrid,
  Stack,
  Text,
  Title,
  Tooltip,
} from '@mantine/core'
import { mdiCheck, mdiContentCopy, mdiDeleteOutline, mdiRefresh, mdiTextBoxOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import relativeTime from 'dayjs/plugin/relativeTime'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { ChallengeBuildAuditModel } from '@Api'
import classes from '@Styles/AdminBuilds.module.css'
import { BUILD_STATUS_COLOR, BUILD_STATUS_VARIANT, formatBuildDuration } from './buildPresentation'

dayjs.extend(relativeTime)

interface BuildHistoryCardProps {
  build: ChallengeBuildAuditModel
  selected: boolean
  busy: boolean
  onSelect: () => void
  onViewLog: () => void
  onReenqueue: () => void
  onDelete: () => void
}

export const BuildHistoryCard: FC<BuildHistoryCardProps> = ({
  build,
  selected,
  busy,
  onSelect,
  onViewLog,
  onReenqueue,
  onDelete,
}) => {
  const { t } = useTranslation()
  const challengeName = build.challengeTitle || `#${build.challengeId}`
  const copyLabel = t('admin.button.builds.copy')
  const copiedLabel = t('admin.button.builds.copied')

  return (
    <Paper component="article" p="md" withBorder className={classes.historyCard} data-selected={selected || undefined}>
      <Stack gap="md">
        <Group align="flex-start" gap="sm" wrap="nowrap">
          <Checkbox
            size="md"
            mt={2}
            checked={selected}
            onChange={onSelect}
            aria-label={t('admin.content.builds.select_one', {
              defaultValue: 'Select build for {{challenge}}',
              challenge: challengeName,
            })}
          />
          <Stack gap={3} miw={0} className={classes.cardIdentity}>
            <Group gap={6} wrap="nowrap" miw={0}>
              <Title order={3} size="sm" className={classes.cardTitle}>
                <Anchor
                  component={Link}
                  to={`/admin/games/${build.gameId}/challenges`}
                  c="var(--app-text-primary)"
                  className={classes.challengeLink}
                >
                  {challengeName}
                </Anchor>
              </Title>
              <Badge size="xs" variant="light" color={build.kind === 'Checker' ? 'grape' : 'gray'}>
                {build.kind === 'Checker'
                  ? t('admin.content.builds.kind.checker', 'checker')
                  : t('admin.content.builds.kind.service', 'service')}
              </Badge>
            </Group>
            <Text size="xs" c="dimmed">
              {dayjs(build.enqueuedAtUtc).fromNow()} ·{' '}
              <time dateTime={build.enqueuedAtUtc}>{dayjs(build.enqueuedAtUtc).format('YYYY-MM-DD HH:mm')}</time>
            </Text>
          </Stack>
          <Badge
            size="sm"
            color={BUILD_STATUS_COLOR[build.status]}
            variant={BUILD_STATUS_VARIANT}
            autoContrast
            className={classes.cardStatus}
          >
            {build.status}
          </Badge>
        </Group>

        <SimpleGrid component="dl" cols={2} spacing="sm" className={classes.cardMetadata}>
          <div>
            <Text component="dt" className={classes.metaLabel}>
              {t('admin.content.builds.column.trigger')}
            </Text>
            <Text component="dd" className={classes.metaValue}>
              {build.trigger}
            </Text>
          </div>
          <div>
            <Text component="dt" className={classes.metaLabel}>
              {t('admin.content.builds.column.duration')}
            </Text>
            <Text component="dd" className={classes.metaValue}>
              {formatBuildDuration(build.durationMs)}
            </Text>
          </div>
          <div>
            <Text component="dt" className={classes.metaLabel}>
              {t('admin.content.builds.column.attempt')}
            </Text>
            <Text component="dd" className={classes.metaValue}>
              {build.attempt}
            </Text>
          </div>
          <div>
            <Text component="dt" className={classes.metaLabel}>
              {t('admin.content.builds.column.image', 'Image')}
            </Text>
            <Text component="dd" className={classes.metaValue}>
              {build.imageRef ? t('admin.content.builds.image_available', 'Available') : '—'}
            </Text>
          </div>
        </SimpleGrid>

        {build.imageRef && (
          <Stack gap={5}>
            <Text className={classes.detailLabel}>{t('admin.content.builds.column.image', 'Image')}</Text>
            <Group gap="xs" wrap="nowrap">
              <Code className={classes.cardReference}>{build.imageRef}</Code>
              <CopyButton value={build.imageRef} timeout={1500}>
                {({ copied, copy }) => (
                  <Tooltip label={copied ? copiedLabel : copyLabel}>
                    <ActionIcon
                      variant="default"
                      color={copied ? 'teal' : 'gray'}
                      aria-label={copied ? copiedLabel : copyLabel}
                      onClick={copy}
                    >
                      <Icon path={copied ? mdiCheck : mdiContentCopy} size={0.8} />
                    </ActionIcon>
                  </Tooltip>
                )}
              </CopyButton>
            </Group>
          </Stack>
        )}

        {(build.errorMessage || build.digest) && (
          <Stack gap={5}>
            <Text className={classes.detailLabel}>{t('admin.content.builds.column.detail')}</Text>
            {build.errorMessage ? (
              <Text size="sm" lineClamp={3} className={classes.errorText} title={build.errorMessage}>
                {build.errorMessage}
              </Text>
            ) : (
              <Code className={classes.cardReference}>{build.digest}</Code>
            )}
          </Stack>
        )}

        <Group gap="xs" wrap="wrap" className={classes.cardActions}>
          <Button
            size="sm"
            variant="default"
            leftSection={<Icon path={mdiTextBoxOutline} size={0.8} />}
            disabled={!build.logTail}
            onClick={onViewLog}
          >
            {t('admin.button.builds.view_log')}
          </Button>
          {(build.status === 'Failed' || build.status === 'MissingDockerfile') && (
            <Button
              size="sm"
              variant="light"
              color="blue"
              leftSection={<Icon path={mdiRefresh} size={0.8} />}
              disabled={busy}
              onClick={onReenqueue}
            >
              {t('admin.button.builds.reenqueue')}
            </Button>
          )}
          <Button
            size="sm"
            variant="light"
            color="red"
            leftSection={<Icon path={mdiDeleteOutline} size={0.8} />}
            disabled={busy}
            onClick={onDelete}
          >
            {t('admin.button.builds.delete')}
          </Button>
        </Group>
      </Stack>
    </Paper>
  )
}
