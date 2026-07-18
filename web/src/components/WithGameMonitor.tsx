import { Button, Flex, LoadingOverlay, Stack, Tabs } from '@mantine/core'
import { useReducedMotion } from '@mantine/hooks'
import { mdiFlag, mdiLightningBolt, mdiPackageVariant, mdiTableArrowDown, mdiGhost } from '@mdi/js'
import { Icon } from '@mdi/react'
import React, { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate, useParams } from 'react-router'
import { WithGameTab } from '@Components/WithGameTab'
import { WithNavBar } from '@Components/WithNavbar'
import { WithRole } from '@Components/WithRole'
import { downloadBlob } from '@Utils/ApiHelper'
import { DEFAULT_LOADING_OVERLAY } from '@Utils/Shared'
import { useIsMobile } from '@Utils/ThemeOverride'
import api, { Role } from '@Api'
import misc from '@Styles/Misc.module.css'

interface WithGameMonitorProps extends React.PropsWithChildren {
  isLoading?: boolean
}

export const WithGameMonitor: FC<WithGameMonitorProps> = ({ children, isLoading }) => {
  const { id } = useParams()
  const numId = parseInt(id ?? '-1')

  const navigate = useNavigate()
  const location = useLocation()
  const { t } = useTranslation()
  const isCompact = useIsMobile(1100)
  const reducedMotion = useReducedMotion()

  const pages = [
    { icon: mdiLightningBolt, title: t('game.tab.monitor.events'), path: 'events' },
    { icon: mdiFlag, title: t('game.tab.monitor.submissions'), path: 'submissions' },
    { icon: mdiGhost, title: t('game.tab.monitor.cheat'), path: 'CheatCheck' },
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

  useEffect(() => {
    if (!isCompact) return

    const scroller = monitorTabsRef.current
    const activeItem = scroller?.querySelector<HTMLElement>('[role="tab"][data-active]')
    if (!scroller || !activeItem) return

    const target = activeItem.offsetLeft - (scroller.clientWidth - activeItem.offsetWidth) / 2
    scroller.scrollTo({ left: Math.max(0, target), behavior: reducedMotion ? 'auto' : 'smooth' })
  }, [activeTab, isCompact, reducedMotion])

  const onDownloadScoreboardSheet = () =>
    downloadBlob(
      api.game.gameScoreboardSheet(numId, { format: 'blob' }),
      setDisabled,
      t,
      `Scoreboard_${numId}_${Date.now()}.xlsx`
    )

  return (
    <WithNavBar>
      <WithRole requiredRole={Role.Monitor}>
        <WithGameTab>
          <Flex direction={isCompact ? 'column' : 'row'} gap="md" justify="space-between" align="flex-start" w="100%">
            <Stack w={isCompact ? '100%' : undefined}>
              <Button
                disabled={disabled}
                w={isCompact ? '100%' : '10rem'}
                classNames={{ inner: misc.justifyBetween }}
                leftSection={<Icon path={mdiTableArrowDown} size={1} />}
                onClick={onDownloadScoreboardSheet}
              >
                {t('game.button.download.scoreboard')}
              </Button>
              <Tabs
                ref={monitorTabsRef}
                orientation={isCompact ? 'horizontal' : 'vertical'}
                value={activeTab}
                onChange={(value) => value && navigate(`/games/${id}/monitor/${value}`)}
                classNames={isCompact ? undefined : { root: misc.w10rem, list: misc.w10rem }}
                w={isCompact ? '100%' : undefined}
                style={isCompact ? { overflowX: 'auto' } : undefined}
              >
                <Tabs.List
                  style={isCompact ? { flexWrap: 'nowrap', width: 'max-content', minWidth: '100%' } : undefined}
                >
                  {pages.map((page) => (
                    <Tabs.Tab key={page.path} leftSection={<Icon path={page.icon} size={1} />} value={page.path}>
                      {page.title}
                    </Tabs.Tab>
                  ))}
                </Tabs.List>
              </Tabs>
            </Stack>
            <Stack
              w={isCompact ? '100%' : 'calc(100% - 11rem)'}
              pos="relative"
              style={{ containerType: 'inline-size' }}
            >
              <LoadingOverlay visible={isLoading ?? false} overlayProps={DEFAULT_LOADING_OVERLAY} />
              {children}
            </Stack>
          </Flex>
        </WithGameTab>
      </WithRole>
    </WithNavBar>
  )
}
