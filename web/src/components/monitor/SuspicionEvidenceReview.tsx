import { Alert, Badge, Box, Button, Card, Divider, Group, Loader, Stack, Table, Text } from '@mantine/core'
import type { FC } from 'react'
import { useTranslation } from 'react-i18next'
import api from '@Api'

interface SuspicionEvidenceReviewProps {
  gameId: number
  eventId: number
}

const ASSESSMENT_META = {
  directEvidence: { color: 'red', label: 'Direct evidence' },
  strongIndicator: { color: 'orange', label: 'Strong indicator' },
  behavioralIndicator: { color: 'yellow', label: 'Behavioral indicator' },
  contextOnly: { color: 'gray', label: 'Context only' },
} as const

const SOURCE_META = {
  verified: { color: 'green', label: 'Verified source' },
  supporting: { color: 'blue', label: 'Supporting records' },
  synthetic: { color: 'violet', label: 'Synthetic demo' },
  unavailable: { color: 'red', label: 'Source unavailable' },
  quarantined: { color: 'gray', label: 'Quarantined legacy event' },
} as const

function displayTime(value?: number | null) {
  return value == null ? '—' : new Date(value).toLocaleString()
}

export const SuspicionEvidenceReviewPanel: FC<SuspicionEvidenceReviewProps> = ({ gameId, eventId }) => {
  const { t } = useTranslation()
  const { data, error, isLoading, mutate } = api.cheatReport.useCheatReportEventEvidence(gameId, eventId, {
    shouldRetryOnError: false,
    revalidateOnFocus: false,
  })

  if (isLoading && !data) {
    return (
      <Group justify="center" py="lg" role="status">
        <Loader size="sm" />
        <Text size="sm" c="dimmed">
          {t('game.cheat_analysis.evidence_loading', 'Loading immutable evidence sources…')}
        </Text>
      </Group>
    )
  }

  if (error || !data) {
    return (
      <Alert color="red" title={t('game.cheat_analysis.evidence_load_failed', 'Evidence could not be loaded')}>
        <Stack gap="xs">
          <Text size="sm">
            {t(
              'game.cheat_analysis.evidence_load_failed_detail',
              'The score row is not enough to support an administrative decision. Retry the source review.'
            )}
          </Text>
          <Button size="xs" variant="outline" color="red" w="fit-content" onClick={() => void mutate()}>
            {t('common.action.retry', 'Retry')}
          </Button>
        </Stack>
      </Alert>
    )
  }

  const assessment = ASSESSMENT_META[data.assessment]
  const source = SOURCE_META[data.sourceStatus]
  const proofVerified = data.isDirectProof && data.sourceStatus === 'verified'
  const downloadReview = () => {
    const url = URL.createObjectURL(new Blob([`${JSON.stringify(data, null, 2)}\n`], { type: 'application/json' }))
    const link = document.createElement('a')
    link.href = url
    link.download = `rsctf-evidence-game-${gameId}-event-${eventId}.json`
    link.click()
    URL.revokeObjectURL(url)
  }

  return (
    <Card withBorder radius="md" padding="md" bg="var(--mantine-color-body)">
      <Stack gap="md">
        <Group justify="space-between" align="flex-start" wrap="wrap">
          <Box>
            <Group gap="xs">
              <Text fw={700}>{data.detectorCode}</Text>
              <Badge color={assessment.color} variant="light">
                {t(`game.cheat_analysis.assessment.${data.assessment}`, assessment.label)}
              </Badge>
              <Badge color={source.color} variant="outline">
                {t(`game.cheat_analysis.source_status.${data.sourceStatus}`, source.label)}
              </Badge>
            </Group>
            <Text size="sm" mt={4}>
              {data.summary}
            </Text>
          </Box>
          <Stack gap={1} align="flex-end">
            <Text size="xs" c="dimmed">
              {t('game.cheat_analysis.observed_at', 'Observed at')}
            </Text>
            <Text size="xs" ff="monospace">
              {displayTime(data.observedAt)}
            </Text>
            <Button size="compact-xs" variant="outline" color="gray" mt={4} onClick={downloadReview}>
              {t('game.cheat_analysis.download_evidence', 'Download evidence JSON')}
            </Button>
          </Stack>
        </Group>

        <Alert
          color={proofVerified ? 'green' : data.assessment === 'contextOnly' ? 'gray' : 'orange'}
          title={
            proofVerified
              ? t('game.cheat_analysis.direct_proof_verified', 'Direct source verified')
              : t('game.cheat_analysis.human_review_required', 'Human review required')
          }
        >
          <Text size="sm">{data.explanation}</Text>
        </Alert>

        <Group gap="xl" align="flex-start" wrap="wrap">
          <Box>
            <Text size="xs" c="dimmed">
              {t('game.cheat_analysis.event_id', 'Event ID')}
            </Text>
            <Text size="sm" ff="monospace">
              #{data.eventId}
            </Text>
          </Box>
          <Box>
            <Text size="xs" c="dimmed">
              {t('game.cheat_analysis.evidence_identity', 'Evidence identity')}
            </Text>
            <Text size="sm" ff="monospace" style={{ overflowWrap: 'anywhere' }}>
              {data.evidenceKey}
            </Text>
          </Box>
          <Box>
            <Text size="xs" c="dimmed">
              {t('game.cheat_analysis.frozen_weight', 'Frozen rule weight')}
            </Text>
            <Text size="sm">{data.scoreDelta}</Text>
          </Box>
        </Group>

        <Divider />
        <Text fw={700} size="sm">
          {t('game.cheat_analysis.source_records', 'Source records')}
        </Text>
        {data.sources.map((record, index) => (
          <Card key={`${record.sourceType}:${record.sourceId ?? index}`} withBorder padding="sm" radius="sm">
            <Stack gap="xs">
              <Group justify="space-between" align="flex-start">
                <Box>
                  <Group gap="xs">
                    <Text fw={650} size="sm">
                      {record.title}
                    </Text>
                    {record.immutable && (
                      <Badge size="xs" color="teal" variant="light">
                        {t('game.cheat_analysis.immutable', 'Immutable')}
                      </Badge>
                    )}
                  </Group>
                  {record.sourceId && (
                    <Text size="xs" c="dimmed" ff="monospace" style={{ overflowWrap: 'anywhere' }}>
                      {record.sourceId}
                    </Text>
                  )}
                </Box>
                <Text size="xs" c="dimmed" ff="monospace">
                  {displayTime(record.recordedAt)}
                </Text>
              </Group>
              <Text size="sm" c="dimmed">
                {record.summary}
              </Text>
              {record.facts.length > 0 && (
                <Table withRowBorders={false} verticalSpacing={3} fz="xs">
                  <Table.Tbody>
                    {record.facts.map((item, factIndex) => (
                      <Table.Tr key={`${item.label}:${factIndex}`}>
                        <Table.Th scope="row" w="12rem" miw="9rem" c="dimmed" fw={500}>
                          {item.label}
                        </Table.Th>
                        <Table.Td ff={item.label.toLowerCase().includes('identity') ? 'monospace' : undefined}>
                          <Text size="xs" style={{ overflowWrap: 'anywhere' }}>
                            {item.value}
                          </Text>
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              )}
            </Stack>
          </Card>
        ))}

        <Group align="flex-start" grow preventGrowOverflow={false} wrap="wrap">
          <Box miw="16rem">
            <Text fw={700} size="sm">
              {t('game.cheat_analysis.limitations', 'Limitations')}
            </Text>
            <Box component="ul" m={0} pl="lg">
              {data.limitations.map((item, index) => (
                <Text component="li" size="xs" c="dimmed" key={index}>
                  {item}
                </Text>
              ))}
            </Box>
          </Box>
          <Box miw="16rem">
            <Text fw={700} size="sm">
              {t('game.cheat_analysis.review_guidance', 'Admin review checklist')}
            </Text>
            <Box component="ul" m={0} pl="lg">
              {data.reviewGuidance.map((item, index) => (
                <Text component="li" size="xs" key={index}>
                  {item}
                </Text>
              ))}
            </Box>
          </Box>
        </Group>
      </Stack>
    </Card>
  )
}
