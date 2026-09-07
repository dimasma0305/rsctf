import { Group, Pagination, Select, Text } from '@mantine/core'
import { mdiCheckCircleOutline, mdiCircleOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { memo, useEffect, useMemo, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useChallengeCategoryLabelMap } from '@Utils/Shared'
import { ChallengeType, type ChallengeInfo } from '@Api'
import classes from './Competition.module.css'
import { challengePage, isLiveChallenge, sortChallenges, type ChallengeSort } from './model'

export const ChallengeList = memo(
  ({
    challenges,
    solvedIds,
    selectedId,
    onSelect,
    sort,
  }: {
    challenges: ChallengeInfo[]
    solvedIds: ReadonlySet<number>
    selectedId?: number
    onSelect: (challenge: ChallengeInfo) => void
    sort: ChallengeSort
  }) => {
    const { t } = useTranslation()
    const categories = useChallengeCategoryLabelMap()
    const [page, setPage] = useState(1)
    const [pageSize, setPageSize] = useState(10)
    const sorted = useMemo(() => sortChallenges(challenges, sort), [challenges, sort])
    const result = challengePage(sorted, page, pageSize)
    const identity = challenges.map((challenge) => challenge.id).join(',')
    useEffect(() => {
      setPage(1)
    }, [identity, sort, pageSize])

    return (
      <section aria-label={t('game.arena.list', 'List')} data-challenge-list data-motion="page">
        <div className={classes.listFrame}>
          <table className={classes.table}>
            <caption className={classes.srOnly}>{t('game.label.challenge_results', 'Challenge list')}</caption>
            <thead>
              <tr>
                <th scope="col">{t('game.arena.challenge', 'Challenge')}</th>
                <th scope="col" className={classes.number}>
                  {t('game.arena.points', 'Points')}
                </th>
                <th scope="col" className={classes.number}>
                  {t('game.arena.solves', 'Solves')}
                </th>
                <th scope="col">{t('game.arena.status', 'Status')}</th>
              </tr>
            </thead>
            <tbody>
              {result.items.map((challenge) => {
                const solved = solvedIds.has(challenge.id)
                const category = categories.get(challenge.category)
                const mode =
                  challenge.type === ChallengeType.KingOfTheHill
                    ? 'KoTH'
                    : challenge.type === ChallengeType.AttackDefense
                      ? 'A&D'
                      : 'Jeopardy'
                return (
                  <tr key={challenge.id} data-selected={challenge.id === selectedId || undefined}>
                    <th scope="row">
                      <button
                        type="button"
                        className={classes.listName}
                        onClick={() => onSelect(challenge)}
                        data-challenge-row={challenge.id}
                        data-guide="challenge-card"
                        aria-pressed={challenge.id === selectedId}
                      >
                        <span>{challenge.title}</span>
                        <small>
                          {category?.name ?? challenge.category} · {mode}
                          {!isLiveChallenge(challenge) && (
                            <span className={classes.mobileSolves}>
                              {' '}
                              · {t('game.arena.solve_count', '{{count}} solves', { count: challenge.solved })}
                            </span>
                          )}
                        </small>
                      </button>
                    </th>
                    <td className={classes.number}>
                      {isLiveChallenge(challenge) ? t('game.arena.live', 'Live') : challenge.score}
                    </td>
                    <td className={classes.number}>{isLiveChallenge(challenge) ? '—' : challenge.solved}</td>
                    <td>
                      <span className={classes.status} data-solved={solved || undefined}>
                        <Icon path={solved ? mdiCheckCircleOutline : mdiCircleOutline} size={0.8} aria-hidden="true" />
                        <span>
                          {solved ? t('common.workspace.solved', 'Solved') : t('game.arena.available', 'Available')}
                        </span>
                      </span>
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
        <Group justify="space-between" mt="sm" gap="xs">
          <Text size="xs" c="dimmed">
            {t('game.arena.page_count', '{{from}}–{{to}} of {{total}}', {
              from: challenges.length ? (result.current - 1) * pageSize + 1 : 0,
              to: Math.min(result.current * pageSize, challenges.length),
              total: challenges.length,
            })}
          </Text>
          <Pagination
            autoContrast
            getControlProps={(control) => ({ 'aria-label': t(`common.pagination.${control}`) })}
            getItemProps={(page) => ({ 'aria-label': t('common.pagination.page', { page }) })}
            total={result.pages}
            value={result.current}
            onChange={setPage}
            size="sm"
            siblings={0}
            boundaries={1}
          />
          <Select
            aria-label={t('game.arena.page_size', 'Challenges per page')}
            w={90}
            size="xs"
            value={String(pageSize)}
            allowDeselect={false}
            data={['10', '25', '50']}
            onChange={(value) => setPageSize(Number(value) || 10)}
          />
        </Group>
      </section>
    )
  }
)
