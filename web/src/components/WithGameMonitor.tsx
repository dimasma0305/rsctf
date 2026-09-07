import { Button, LoadingOverlay, Stack, Tabs } from '@mantine/core'
import { mdiFlag, mdiLightningBolt, mdiPackageVariant, mdiTableArrowDown, mdiGhost } from '@mdi/js'
import { Icon } from '@mdi/react'
import React, { FC, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate, useParams } from 'react-router'
import { WithGameTab } from '@Components/WithGameTab'
import { GAME_PAGE_CONTENT_WIDTH, WithNavBar } from '@Components/WithNavbar'
import { WithRole } from '@Components/WithRole'
import { downloadBlob } from '@Utils/ApiHelper'
import { DEFAULT_LOADING_OVERLAY } from '@Utils/Shared'
import api, { Role } from '@Api'
import classes from '@Styles/WithGameMonitor.module.css'

interface WithGameMonitorProps extends React.PropsWithChildren {
  isLoading?: boolean
}

export const WithGameMonitor: FC<WithGameMonitorProps> = ({ children, isLoading }) => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')

  const navigate = useNavigate()
  const location = useLocation()
  const { t } = useTranslation()

  const pages = [
    { icon: mdiLightningBolt, title: t('game.tab.monitor.events'), path: 'events' },
    { icon: mdiFlag, title: t('game.tab.monitor.submissions'), path: 'submissions' },
    { icon: mdiGhost, title: t('game.tab.monitor.cheat'), path: 'cheatcheck' },
    { icon: mdiPackageVariant, title: t('game.tab.monitor.traffic'), path: 'traffic' },
  ]

  const getTab = (path: string) => pages.find((page) => path.endsWith(page.path))

  const [activeTab, setActiveTab] = useState(getTab(location.pathname)?.path ?? pages[0].path)
  const [disabled, setDisabled] = useState(false)
  const monitorTabsRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const tab = getTab(location.pathname)
    if (tab) {
      setActiveTab(tab.path ?? '')
    } else {
      navigate(`/games/${id}/monitor/${pages[0].path}`, { replace: true })
    }
  }, [id, location.pathname, navigate])

  useLayoutEffect(() => {
    const scroller = monitorTabsRef.current
    const activeItem = scroller?.querySelector<HTMLElement>('[role="tab"][data-active]')
    if (!scroller || !activeItem) return

    const viewport = scroller.getBoundingClientRect()
    const item = activeItem.getBoundingClientRect()
    const overflow =
      item.left < viewport.left
        ? item.left - viewport.left
        : item.right > viewport.right
          ? item.right - viewport.right
          : 0
    if (overflow !== 0) scroller.scrollTo({ left: Math.max(0, scroller.scrollLeft + overflow), behavior: 'auto' })
  }, [activeTab])

  const onDownloadScoreboardSheet = () =>
    downloadBlob(
      `monitor:scoreboard:${numId}`,
      () => api.game.gameScoreboardSheet(numId, { format: 'blob' }),
      setDisabled,
      t,
      `Scoreboard_${numId}_${Date.now()}.xlsx`
    )

  return (
    <WithNavBar width={GAME_PAGE_CONTENT_WIDTH} competition>
      <WithRole requiredRole={Role.Monitor}>
        <WithGameTab>
          <Stack gap="md" w="100%">
            <div className={classes.toolbar}>
              <Tabs
                ref={monitorTabsRef}
                value={activeTab}
                onChange={(value) => value && navigate(`/games/${id}/monitor/${value}`)}
                classNames={{ root: classes.tabs, list: classes.tabList }}
              >
                <Tabs.List aria-label={t('game.tab.monitor.index')}>
                  {pages.map((page) => (
                    <Tabs.Tab
                      key={page.path}
                      leftSection={<Icon path={page.icon} size={0.85} aria-hidden="true" />}
                      value={page.path}
                    >
                      {page.title}
                    </Tabs.Tab>
                  ))}
                </Tabs.List>
              </Tabs>
              <Button
                disabled={disabled}
                variant="default"
                className={classes.exportButton}
                leftSection={<Icon path={mdiTableArrowDown} size={0.85} aria-hidden="true" />}
                onClick={onDownloadScoreboardSheet}
              >
                {t('game.button.export_scoreboard')}
              </Button>
            </div>
            <Stack w="100%" pos="relative" style={{ containerType: 'inline-size' }}>
              <LoadingOverlay visible={isLoading ?? false} overlayProps={DEFAULT_LOADING_OVERLAY} />
              {children}
            </Stack>
          </Stack>
        </WithGameTab>
      </WithRole>
    </WithNavBar>
  )
}
