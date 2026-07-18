import { CheatInfo } from '@Components/monitor/CheatInfo'
import { CheatSubmissionLog } from '@Components/monitor/CheatSubmissionLog'
import { Loader, Stack, Title, Alert, Tabs, Text, Group, ThemeIcon, Box } from '@mantine/core'
import { FC, useState } from 'react'
import { useParams, useSearchParams } from 'react-router'
import { WithGameMonitor } from '@Components/WithGameMonitor'
import { tryGetErrorMsg } from '@Utils/Shared'
import api from '@Api'
import { useTranslation } from 'react-i18next'
import { Icon } from '@mdi/react'
import { mdiChartBox, mdiFlagVariant, mdiShieldSearch, mdiAlertCircle } from '@mdi/js'

const CheatCheck: FC = () => {
    const { id } = useParams()
    const numId = parseInt(id!)
    const { t } = useTranslation()
    // Tab state
    const [searchParams, setSearchParams] = useSearchParams()
    const tabFromUrl = searchParams.get('tab')
    const [activeTab, setActiveTab] = useState<string | null>(tabFromUrl || 'analysis')

    // Handle tab change and update URL
    const handleTabChange = (value: string | null) => {
        setActiveTab(value)
        setSearchParams({ tab: value || 'analysis' })
    }

    // Api call (for Analysis view)
    const { data: report, isLoading, error, mutate } = api.cheatReport.useCheatReportGet(numId, {
        revalidateOnFocus: false,
        revalidateOnReconnect: false,
        refreshInterval: 0,
    })

    if (isLoading)
        return (
            <WithGameMonitor>
                <Stack align="center" justify="center" h="60vh" gap="md">
                    <Loader size="lg" />
                    <Text c="dimmed" size="sm">
                        {t('game.content.cheat.loading', 'Loading cheat analysis…')}
                    </Text>
                </Stack>
            </WithGameMonitor>
        )

    if (error)
        return (
            <WithGameMonitor>
                <Alert
                    color="alert"
                    title={t('game.content.cheat.load_failed', 'Failed to load report')}
                    icon={<Icon path={mdiAlertCircle} size={1} />}
                >
                    {tryGetErrorMsg(error, t)}
                </Alert>
            </WithGameMonitor>
        )

    return (
        <WithGameMonitor>
            <Stack gap="md" w="100%">
                {/* ── Page header ──────────────────────── */}
                <Group gap="sm" align="center">
                    <ThemeIcon size="lg" radius="md" variant="light" color="alert">
                        <Icon path={mdiShieldSearch} size={0.9} />
                    </ThemeIcon>
                    <Box>
                        <Title order={3}>{t('game.title.cheat_check', 'Cheat Analysis')}</Title>
                        <Text size="xs" c="dimmed">
                            {t(
                                'game.content.cheat.subtitle',
                                'Behavioral analysis, IP anomalies, and flag-sharing detection'
                            )}
                        </Text>
                    </Box>
                </Group>

                {/* ── Top-level tabs ────────────────────── */}
                <Tabs
                    value={activeTab}
                    onChange={handleTabChange}
                    variant="pills"
                    radius="md"
                >
                    <Tabs.List
                        style={{
                            borderBottom: '1px solid light-dark(var(--mantine-color-gray-2), var(--mantine-color-dark-5))',
                            paddingBottom: 4,
                            marginBottom: 8,
                        }}
                    >
                        <Tabs.Tab
                            value="analysis"
                            leftSection={<Icon path={mdiChartBox} size={0.85} />}
                        >
                            {t('game.tab.cheat.analysis', 'Anomaly Analysis')}
                        </Tabs.Tab>
                        <Tabs.Tab
                            value="submissions"
                            leftSection={<Icon path={mdiFlagVariant} size={0.85} />}
                        >
                            {t('game.tab.cheat.submissions', 'Submissions & Flags')}
                        </Tabs.Tab>
                    </Tabs.List>

                    <Tabs.Panel value="analysis" pt="xs">
                        <CheatInfo report={report || null} mutate={mutate} />
                    </Tabs.Panel>

                    <Tabs.Panel value="submissions" pt="xs">
                        <CheatSubmissionLog gameId={numId} />
                    </Tabs.Panel>
                </Tabs>
            </Stack>
        </WithGameMonitor>
    )
}

export default CheatCheck
