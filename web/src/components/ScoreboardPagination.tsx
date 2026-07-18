import { Box, Group, Pagination, VisuallyHidden } from '@mantine/core'
import { useMediaQuery } from '@mantine/hooks'
import { FC, useEffect } from 'react'
import { useTranslation } from 'react-i18next'
import { getPaginationState } from '@Utils/PaginationState'
import classes from '@Styles/ScoreboardPagination.module.css'

interface ScoreboardPaginationProps {
  value: number
  onChange: (page: number) => void
  total: number
  boundaries?: number
}

/**
 * Shared scoreboard pagination with direct page selection on wider screens
 * and a compact status on narrow screens. It remains visible for a single
 * page so scoreboard footers do not shift as filters and data change.
 */
export const ScoreboardPagination: FC<ScoreboardPaginationProps> = ({ value, onChange, total, boundaries = 1 }) => {
  const { t } = useTranslation()
  const compact = useMediaQuery('(max-width: 47.99em)', false, { getInitialValueInEffect: false })
  const { page, totalPages } = getPaginationState(value, total)

  useEffect(() => {
    if (page !== value) onChange(page)
  }, [onChange, page, value])

  const previousLabel = t('common.pagination.previous', 'Previous page')
  const nextLabel = t('common.pagination.next', 'Next page')
  const pageStatus = t('common.pagination.page_of', {
    defaultValue: 'Page {{page}} of {{total}}',
    page,
    total: totalPages,
  })

  return (
    <Box
      component="nav"
      aria-label={t('game.content.scoreboard.pagination_label', 'Scoreboard result pages')}
      className={classes.nav}
    >
      <Pagination.Root
        value={page}
        onChange={onChange}
        total={totalPages}
        boundaries={compact ? 1 : Math.max(0, Math.floor(boundaries))}
        siblings={compact ? 0 : 1}
        size="xl"
        autoContrast
        className={classes.pagination}
        getItemProps={(page) => ({
          'aria-label': t('common.pagination.page', {
            defaultValue: 'Page {{page}}',
            page,
          }),
        })}
      >
        <Group gap={compact ? 4 : 6} wrap="nowrap" className={classes.track}>
          <Pagination.Previous aria-label={previousLabel} title={previousLabel} />
          {compact ? (
            <Box className={classes.status} aria-hidden="true">
              <span className={classes.currentPage}>{page}</span>
              <span className={classes.separator}>/</span>
              <span>{totalPages}</span>
            </Box>
          ) : (
            <Pagination.Items />
          )}
          <Pagination.Next aria-label={nextLabel} title={nextLabel} />
        </Group>
      </Pagination.Root>
      <VisuallyHidden aria-live="polite" aria-atomic="true">
        {pageStatus}
      </VisuallyHidden>
    </Box>
  )
}
