import { Button, Group, Popover, SegmentedControl, Select, Stack, Switch, Tabs, Text, TextInput } from '@mantine/core'
import { mdiEarth, mdiFileUploadOutline, mdiFilterVariant, mdiFormatListBulleted, mdiViewGridOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { useTranslation } from 'react-i18next'
import { useChallengeCategoryLabelMap } from '@Utils/Shared'
import type { ChallengeCategory } from '@Api'
import classes from '@Styles/ChallengePanel.module.css'
import type { ChallengeSort, ChallengeView } from './model'

type Kind = 'all' | 'jeopardy' | 'ad' | 'koth'

// Presentation only: the panel remains the single owner of filtering and reads.
export function ChallengeToolbar(props: {
  search: string
  onSearch: (value: string) => void
  view: ChallengeView
  onView: (value: ChallengeView) => void
  sort: ChallengeSort
  onSort: (value: ChallengeSort) => void
  category: ChallengeCategory | 'All'
  onCategory: (value: ChallengeCategory | 'All') => void
  categories: { value: ChallengeCategory; count: number }[]
  kind: Kind
  onKind: (value: Kind) => void
  kinds: Exclude<Kind, 'all'>[]
  hideSolved: boolean
  onHideSolved: (value: boolean) => void
  shown: number
  total: number
  onReset: () => void
  onWriteup?: () => void
}) {
  const { t } = useTranslation()
  const categoryLabels = useChallengeCategoryLabelMap()
  const activeFilters = Number(props.hideSolved) + Number(props.category !== 'All') + Number(props.kind !== 'all')
  const categoryOptions = [
    { value: 'All', label: t('game.button.kind.all', 'All'), count: props.total },
    ...props.categories.map((item) => ({ ...item, label: categoryLabels.get(item.value)?.name ?? item.value })),
  ]
  const sortControl = (
    <Select
      label={t('game.arena.sort', 'Sort by')}
      value={props.sort}
      allowDeselect={false}
      comboboxProps={{ withinPortal: false }}
      onChange={(value) => {
        if (value) props.onSort(value as ChallengeSort)
      }}
      data={[
        { value: 'name', label: t('game.arena.name', 'Name') },
        { value: 'score', label: t('game.arena.points', 'Points') },
        { value: 'solves', label: t('game.arena.solves', 'Solves') },
      ]}
    />
  )
  return (
    <div className={classes.toolbar} data-challenge-toolbar>
      <div className={classes.toolbarMain}>
        <TextInput
          className={classes.search}
          label={t('common.workspace.search_challenges', 'Find a challenge')}
          placeholder={t('common.workspace.challenge_placeholder', 'Name or ID')}
          value={props.search}
          onChange={(event) => props.onSearch(event.currentTarget.value)}
        />
        {props.view === 'list' && <div className={classes.desktopSort}>{sortControl}</div>}
        <SegmentedControl
          aria-label={t('game.arena.view', 'Challenge view')}
          value={props.view}
          onChange={(value) => props.onView(value as ChallengeView)}
          className={classes.viewControl}
          data={[
            ['globe', mdiEarth, t('game.arena.globe_short', 'Globe')],
            ['list', mdiFormatListBulleted, t('game.arena.list', 'List')],
            ['cards', mdiViewGridOutline, t('game.arena.cards', 'Cards')],
          ].map(([value, icon, label]) => ({
            value,
            label: (
              <Group gap={4} wrap="nowrap">
                <Icon path={icon} size={0.75} aria-hidden="true" />
                {label}
              </Group>
            ),
          }))}
        />
        <Popover
          position="bottom-end"
          width="min(20rem, calc(100vw - 2rem))"
          trapFocus
          returnFocus
          withArrow
          withinPortal={false}
        >
          <Popover.Target>
            <Button
              data-challenge-filters
              className={classes.filterButton}
              variant={activeFilters ? 'light' : 'default'}
              leftSection={<Icon path={mdiFilterVariant} size={0.8} aria-hidden="true" />}
            >
              {t('common.workspace.filters', 'Filters')}
              {activeFilters ? ` · ${activeFilters}` : ''}
            </Button>
          </Popover.Target>
          <Popover.Dropdown>
            <Stack gap="md">
              <Select
                id="challenge-category-filter"
                label={t('game.label.challenge_category', 'Category')}
                value={props.category}
                data={categoryOptions}
                allowDeselect={false}
                comboboxProps={{ withinPortal: false }}
                onChange={(value) => {
                  if (value) props.onCategory(value as ChallengeCategory | 'All')
                }}
              />
              {props.kinds.length >= 2 && (
                <Select
                  label={t('game.label.challenge_type', 'Challenge type')}
                  value={props.kind}
                  allowDeselect={false}
                  comboboxProps={{ withinPortal: false }}
                  onChange={(value) => {
                    if (value) props.onKind(value as Kind)
                  }}
                  data={['all' as const, ...props.kinds].map((value) => ({
                    value,
                    label: t(`game.button.kind.${value}`, {
                      defaultValue: { all: 'All', jeopardy: 'CTF', ad: 'A&D', koth: 'KotH' }[value],
                    }),
                  }))}
                />
              )}
              {props.view === 'list' && <div className={classes.mobileSort}>{sortControl}</div>}
              <Switch
                checked={props.hideSolved}
                onChange={(event) => props.onHideSolved(event.currentTarget.checked)}
                label={t('game.button.hide_solved')}
              />
              <Button variant="subtle" onClick={props.onReset}>
                {t('common.workspace.reset_filters', 'Reset filters')}
              </Button>
            </Stack>
          </Popover.Dropdown>
        </Popover>
        {props.onWriteup && (
          <Button
            variant="light"
            className={classes.writeupButton}
            leftSection={<Icon path={mdiFileUploadOutline} size={0.8} aria-hidden="true" />}
            onClick={props.onWriteup}
          >
            {t('game.button.submit_writeup')}
          </Button>
        )}
      </div>
      <div className={classes.toolbarSecondary}>
        <Tabs
          autoContrast
          variant="pills"
          value={props.category}
          onChange={(value) => {
            if (value) props.onCategory(value as ChallengeCategory | 'All')
          }}
          classNames={{ root: classes.tabRoot, list: classes.tabList, tab: classes.tab }}
        >
          <Tabs.List data-challenge-category-tabs aria-label={t('game.label.challenge_category', 'Filter by category')}>
            {categoryOptions.map((item) => (
              <Tabs.Tab key={item.value} value={item.value}>
                {item.label} <span className={classes.categoryCount}>{item.count}</span>
              </Tabs.Tab>
            ))}
          </Tabs.List>
        </Tabs>
        <Group gap="xs" wrap="wrap" className={classes.resultCount}>
          <Text size="xs" c="dimmed" role="status">
            {t('common.workspace.challenge_count', '{{shown}} of {{total}} challenges', {
              shown: props.shown,
              total: props.total,
            })}
          </Text>
          {(props.search || activeFilters > 0) && (
            <Button variant="subtle" size="compact-xs" onClick={props.onReset}>
              {t('common.workspace.reset_filters', 'Reset filters')}
            </Button>
          )}
        </Group>
      </div>
    </div>
  )
}
