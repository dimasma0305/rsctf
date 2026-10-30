import { Button } from '@mantine/core'
import { useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router'
import { RetryableMutationOwner } from '@Utils/RetryableMutationOwner'
import { showErrorMsg } from '@Utils/Shared'
import { createUuid } from '@Utils/Uuid'
import api, { ChallengeCategory, ChallengeType } from '@Api'

/** Retries retain both identities, including when only the game was committed. */
export function SampleGameButton() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const owner = useRef(new RetryableMutationOwner())
  const draft = useRef<{ start: number; gameOperation: string; challengeOperation: string; gameId?: number } | null>(
    null
  )
  const [busy, setBusy] = useState(false)
  useEffect(() => () => owner.current.cancel(), [])
  const create = async () => {
    const lease = owner.current.claim('sample-game')
    if (!lease) return
    setBusy(true)
    const saved = (draft.current ??= {
      start: Date.now() + 86_400_000,
      gameOperation: createUuid(),
      challengeOperation: createUuid(),
    })
    try {
      if (!saved.gameId) {
        const result = await api.edit.editAddGame(
          { title: 'My first game', hidden: true, sourceRevision: 0, start: saved.start, end: saved.start + 7_200_000 },
          saved.gameOperation,
          { signal: lease.signal }
        )
        if (!owner.current.owns(lease)) return
        saved.gameId = result.data.id
      }
      if (!saved.gameId) throw new Error('The game could not be created. Please try again.')
      await api.edit.editAddGameChallenge(
        saved.gameId,
        {
          title: 'Find your first flag',
          category: ChallengeCategory.Misc,
          type: ChallengeType.StaticAttachment,
          content:
            'Welcome! A flag is an answer you submit to earn points.\n\nCopy this flag into the answer box: `flag{hello_world}`',
          initialFlag: 'flag{hello_world}',
          originalScore: 100,
          operationId: saved.challengeOperation,
        },
        { signal: lease.signal }
      )
      if (!owner.current.settle(lease, true)) return
      navigate(`/admin/games/${saved.gameId}/setup`)
    } catch (error) {
      if (!owner.current.settle(lease, false)) return
      showErrorMsg(error, t)
      setBusy(false)
    }
  }
  return (
    <Button variant="light" h={44} loading={busy} onClick={() => void create()}>
      {t('admin.easy.sample', 'Create sample game')}
    </Button>
  )
}
