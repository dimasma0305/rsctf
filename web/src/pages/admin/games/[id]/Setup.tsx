import { Alert, Button, Group, Paper, Stack, Text, Title } from '@mantine/core'
import { useClipboard } from '@mantine/hooks'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useParams } from 'react-router'
import { RequestError } from '@Components/RequestError'
import { AdminPage } from '@Components/admin/AdminPage'
import { ChallengeCreateModal } from '@Components/admin/ChallengeCreateModal'
import { SimpleChallengeTest } from '@Components/admin/SimpleChallengeTest'
import { OnceSWRConfig } from '@Hooks/useConfig'
import api, { ChallengeInfoModel, ChallengeType } from '@Api'

export default function GameSetup() {
  const id = Number(useParams().id)
  const { t } = useTranslation()
  const game = api.edit.useEditGetGame(id, OnceSWRConfig)
  const challenges = api.edit.useEditGetGameChallenges(id, OnceSWRConfig)
  const [creating, setCreating] = useState(false)
  const [testing, setTesting] = useState<ChallengeInfoModel | null>(null)
  const clipboard = useClipboard()
  const root = `/admin/games/${id}`
  const failed = game.error || challenges.error
  const loading = !failed && (!game.data || !challenges.data)
  return (
    <AdminPage
      isLoading={loading}
      headerActions={
        <Button component={Link} to={`${root}/info`} variant="default">
          {t('admin.easy.advanced_settings', 'Advanced settings')}
        </Button>
      }
    >
      {failed ? (
        <RequestError name={t('common.label.game')} onRetry={() => Promise.all([game.mutate(), challenges.mutate()])} />
      ) : (
        <Stack gap="lg" aria-busy={loading} style={{ visibility: loading ? 'hidden' : undefined }}>
          <Paper withBorder p="md">
            <Stack>
              <Title order={2}>{t('admin.easy.step1', '1. Add a challenge')}</Title>
              <Text>{t('admin.easy.step1_hint', 'Write a question, choose its answer, and set the points.')}</Text>
              <Group>
                <Button onClick={() => setCreating(true)}>{t('admin.button.challenges.new')}</Button>
                <Button component={Link} to={`${root}/info`} variant="default">
                  {t('admin.easy.schedule', 'Change game name or dates')}
                </Button>
              </Group>
            </Stack>
          </Paper>
          <Paper withBorder p="md">
            <Stack>
              <Title order={2}>{t('admin.easy.step2', '2. Try it and enable it')}</Title>
              <Text>
                {t(
                  'admin.easy.step2_hint',
                  'Try the answer before players arrive. New challenges stay disabled until you enable them.'
                )}
              </Text>
              {challenges.data?.map((challenge) => (
                <Group key={challenge.id} justify="space-between">
                  <Text style={{ overflowWrap: 'anywhere' }}>{challenge.title}</Text>
                  <Group>
                    {challenge.type === ChallengeType.StaticAttachment && (
                      <Button variant="light" onClick={() => setTesting(challenge)}>
                        {t('admin.easy.test', 'Try answer')}
                      </Button>
                    )}
                    <Button variant="default" component={Link} to={`${root}/challenges/${challenge.id}`}>
                      {t('admin.easy.edit', 'Edit challenge')}
                    </Button>
                  </Group>
                </Group>
              ))}
              <Button variant="light" component={Link} to={`${root}/readiness`}>
                {t('admin.easy.check', 'Check game readiness')}
              </Button>
            </Stack>
          </Paper>
          <Paper withBorder p="md">
            <Stack>
              <Title order={2}>{t('admin.easy.step3', '3. Share your game')}</Title>
              <Text>
                {t(
                  'admin.easy.step3_hint',
                  'Players open this link, sign in, and choose their team. Check the dates and team approval settings before inviting everyone.'
                )}
              </Text>
              {game.data?.hidden && (
                <Alert>
                  {t('admin.easy.unlisted', 'This game is unlisted. Anyone with its link can still open it.')}
                </Alert>
              )}
              <Group>
                <Button onClick={() => clipboard.copy(`${window.location.origin}/games/${id}`)}>
                  {clipboard.copied ? t('admin.easy.copied', 'Copied!') : t('admin.easy.copy', 'Copy game link')}
                </Button>
                <Button component={Link} to={`/games/${id}`} variant="default">
                  {t('admin.easy.open', 'Open game')}
                </Button>
              </Group>
            </Stack>
          </Paper>
        </Stack>
      )}
      <ChallengeCreateModal
        title={t('admin.button.challenges.new')}
        opened={creating}
        onClose={() => setCreating(false)}
        returnToSetup
        onAddChallenge={() => void challenges.mutate()}
      />
      {testing?.id && (
        <SimpleChallengeTest
          gameId={id}
          challengeId={testing.id}
          opened
          onClose={() => setTesting(null)}
          onEnabled={() => void challenges.mutate()}
        />
      )}
    </AdminPage>
  )
}
