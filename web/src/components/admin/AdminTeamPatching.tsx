import { Badge, Divider, Group, Paper, SimpleGrid, Stack, Switch, Text, ThemeIcon, Title } from '@mantine/core'
import { mdiArchiveOutline, mdiConsole } from '@mdi/js'
import { Icon } from '@mdi/react'
import { useEffect, useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { AdPatchStarterInput } from './AdPatchStarterInput'

interface Props {
  gameId: number
  challengeId: number
  isKoth: boolean
  savedByoc: boolean
  byoc: boolean
  requiresFlag: boolean
  started: boolean
  settingsDisabled: boolean
  uploadDisabled: boolean
  onByocChange: (value: boolean) => void
  onFlagChange: (value: boolean) => void
}

export function AdminTeamPatching({
  gameId, challengeId, isKoth, savedByoc, byoc, requiresFlag, started,
  settingsDisabled, uploadDisabled, onByocChange, onFlagChange,
}: Props) {
  const { t } = useTranslation()
  const heading = useRef<HTMLHeadingElement>(null)
  useEffect(() => {
    if (window.location.hash === '#team-patching') heading.current?.focus()
  }, [gameId, challengeId])
  return (
    <Paper component="section" aria-labelledby="team-patching" p="lg" withBorder>
      <Stack gap="md">
        <Group justify="space-between">
          <Title ref={heading} order={3} id="team-patching" tabIndex={-1} style={{ scrollMarginTop: '6rem' }}>
            {t('admin.ad.team_patching.title', 'Team patching')}
          </Title>
          <Badge variant="light" color={isKoth ? 'violet' : byoc ? 'blue' : 'teal'}>
            {isKoth ? t('admin.content.games.challenges.koth.title', 'King of the Hill')
              : byoc ? 'BYOC' : t('admin.ad.team_patching.hosted', 'RSCTF-hosted')}
          </Badge>
        </Group>
        {isKoth ? (
          <Text size="sm">{t('admin.ad.team_patching.koth', 'Teams compete on one shared hill. For per-team SSH and ZIP patches, create a hosted Attack & Defense challenge.')}</Text>
        ) : (
          <>
            <Switch label={t('admin.content.games.challenges.ad.self_hosted.label', 'Self-hosted / Bring Your Own Container (BYOC)')}
              checked={byoc} disabled={settingsDisabled} onChange={event => onByocChange(event.currentTarget.checked)} />
            <Divider />
            <SimpleGrid cols={{ base: 1, md: 2 }} spacing="xl" verticalSpacing="lg">
              <Stack gap="xs" miw={0}>
                <Group gap="xs">
                  <ThemeIcon variant="light" radius="md" aria-hidden="true"><Icon path={mdiArchiveOutline} size={0.85} /></ThemeIcon>
                  <Title order={4}>{t('admin.ad.team_patching.zip', 'ZIP patches')}</Title>
                </Group>
                {byoc ? (
                  <Text size="sm">{t('admin.ad.team_patching.byoc_zip', 'Teams patch on their own host. Starter ZIP uploads are available for RSCTF-hosted A&D challenges.')}</Text>
                ) : savedByoc ? (
                  <Text size="sm" role="status">{t('admin.ad.team_patching.save_hosting', 'Save the hosting change to upload a starter ZIP.')}</Text>
                ) : (
                  <AdPatchStarterInput key={`${gameId}:${challengeId}`} gameId={gameId} challengeId={challengeId} started={started} disabled={uploadDisabled} />
                )}
              </Stack>
              <Stack gap="xs" miw={0}>
                <Group gap="xs">
                  <ThemeIcon variant="light" radius="md" color="teal" aria-hidden="true"><Icon path={mdiConsole} size={0.85} /></ThemeIcon>
                  <Title order={4}>{t('admin.ad.team_patching.ssh', 'SSH access')}</Title>
                </Group>
                <Text size="sm">
                  {byoc ? t('admin.ad.team_patching.byoc_ssh', 'Teams manage shell access on their host. Toolkit SSH requires the optional BYOC shell.')
                    : t('admin.ad.team_patching.hosted_ssh', 'Players register their SSH key in the A&D Toolkit to edit their team’s running service.')}
                </Text>
                <Switch label={t('admin.content.games.challenges.ad.ssh_requires_flag.label', 'SSH requires a captured flag')}
                  checked={requiresFlag} disabled={settingsDisabled} onChange={event => onFlagChange(event.currentTarget.checked)} />
              </Stack>
            </SimpleGrid>
          </>
        )}
      </Stack>
    </Paper>
  )
}
