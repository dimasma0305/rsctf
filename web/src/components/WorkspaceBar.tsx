import { Anchor, Button, Kbd, Modal, Stack, Text, TextInput } from '@mantine/core'
import { mdiArrowRight, mdiChevronRight, mdiMagnify } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useLocation } from 'react-router'
import { getAdminNavigation, PRIMARY_NAVIGATION, canAccessNavigationItem } from '@Components/navigation'
import { useConfig } from '@Hooks/useConfig'
import { useUser } from '@Hooks/useUser'
import classes from '@Styles/WorkspaceBar.module.css'

export const WorkspaceBar: FC = () => {
  const { t } = useTranslation()
  const location = useLocation()
  const { user } = useUser()
  const { config } = useConfig()
  const [opened, setOpened] = useState(false)
  const [query, setQuery] = useState('')
  const resultsRef = useRef<HTMLDivElement>(null)
  const isAdmin = location.pathname.startsWith('/admin/')
  const eventId = location.pathname.match(/^\/(?:admin\/)?games\/(\d+)(?:\/|$)/)?.[1]
  const items = useMemo(
    () => [
      ...PRIMARY_NAVIGATION.filter(
        (item) => !item.admin && canAccessNavigationItem(item, user, config.donationsEnabled)
      ).map((item) => ({ ...item, title: t(item.label), section: t('common.workspace.player', 'Player workspace') })),
      ...getAdminNavigation(user).map((item) => ({
        ...item,
        link: `/admin/${item.path}`,
        title: t(item.label, item.fallback),
        section: t('common.workspace.admin', 'Administration'),
      })),
    ],
    [user, config.donationsEnabled, t]
  )
  const results = items.filter((item) =>
    `${item.title} ${item.section} ${item.link}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())
  )
  const active = items
    .filter(
      (item) => item.link !== '/' && (location.pathname === item.link || location.pathname.startsWith(`${item.link}/`))
    )
    .sort((a, b) => b.link.length - a.link.length)[0]

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k' && !event.altKey && !event.isComposing) {
        // Never cover a confirmation, challenge, or credential dialog.
        const visibleDialog = Array.from(document.querySelectorAll('[role="dialog"], dialog[open]')).some(
          (dialog) => dialog.getClientRects().length > 0
        )
        if (!opened && visibleDialog) return
        event.preventDefault()
        setOpened((value) => !value)
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [opened])

  useEffect(() => {
    setOpened(false)
    setQuery('')
  }, [location.pathname])

  return (
    <>
      <div className={classes.bar} data-workspace-bar>
        <nav aria-label={t('common.workspace.breadcrumbs', 'Breadcrumbs')} className={classes.breadcrumbs}>
          <Anchor component={Link} to={isAdmin ? '/admin/games' : '/games'}>
            {isAdmin ? t('common.workspace.admin', 'Administration') : t('common.workspace.player', 'Player workspace')}
          </Anchor>
          {active && (
            <>
              <Icon path={mdiChevronRight} size={0.65} aria-hidden="true" />
              <Text size="xs">{active.title}</Text>
            </>
          )}
          {eventId && (
            <>
              <Icon path={mdiChevronRight} size={0.65} aria-hidden="true" />
              <Anchor component={Link} to={isAdmin ? `/admin/games/${eventId}/info` : `/games/${eventId}`}>
                {t('common.workspace.event_id', 'Event #{{id}}', { id: eventId })}
              </Anchor>
            </>
          )}
        </nav>
        <Button
          variant="default"
          size="xs"
          className={classes.trigger}
          leftSection={<Icon path={mdiMagnify} size={0.8} aria-hidden="true" />}
          onClick={() => {
            setQuery('')
            setOpened(true)
          }}
          aria-haspopup="dialog"
          aria-keyshortcuts="Control+k Meta+k"
        >
          {t('common.workspace.jump', 'Go to…')} <Kbd className={classes.shortcut}>Ctrl / ⌘ K</Kbd>
        </Button>
      </div>
      <Modal
        opened={opened}
        onClose={() => setOpened(false)}
        title={t('common.workspace.quick_navigation', 'Go to a page')}
        size="lg"
        closeButtonProps={{ 'aria-label': t('common.button.close', 'Close') }}
      >
        <Stack gap="md">
          <TextInput
            data-autofocus
            label={t('common.workspace.search_label', 'Search pages')}
            placeholder={t('common.workspace.search_placeholder', 'Events, teams, settings…')}
            value={query}
            onChange={(event) => setQuery(event.currentTarget.value)}
            leftSection={<Icon path={mdiMagnify} size={0.9} aria-hidden="true" />}
            onKeyDown={(event) => {
              if (event.key === 'ArrowDown') {
                event.preventDefault()
                resultsRef.current?.querySelector<HTMLElement>('a')?.focus()
              }
            }}
          />
          <Text size="xs" c="dimmed" role="status">
            {t('common.workspace.result_count', '{{count}} pages', { count: results.length })}
          </Text>
          <div
            ref={resultsRef}
            className={classes.results}
            onKeyDown={(event) => {
              if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return
              const links = Array.from(resultsRef.current?.querySelectorAll<HTMLAnchorElement>('a') ?? [])
              const current = links.indexOf(document.activeElement as HTMLAnchorElement)
              if (current < 0 || !links.length) return
              event.preventDefault()
              const next =
                event.key === 'Home'
                  ? 0
                  : event.key === 'End'
                    ? links.length - 1
                    : (current + (event.key === 'ArrowDown' ? 1 : -1) + links.length) % links.length
              links[next]?.focus()
            }}
          >
            {results.map((item) => (
              <Link key={item.link} to={item.link} className={classes.result} onClick={() => setOpened(false)}>
                <Icon path={item.icon} size={0.95} aria-hidden="true" />
                <Stack gap={1} miw={0}>
                  <Text size="sm" fw={600}>
                    {item.title}
                  </Text>
                  <Text size="xs" c="dimmed">
                    {item.section}
                  </Text>
                </Stack>
                <Icon className={classes.arrow} path={mdiArrowRight} size={0.75} aria-hidden="true" />
              </Link>
            ))}
            {!results.length && (
              <Text size="sm" c="dimmed" py="lg">
                {t('common.workspace.no_results', 'No matching pages. Try a different name.')}
              </Text>
            )}
          </div>
        </Stack>
      </Modal>
    </>
  )
}
