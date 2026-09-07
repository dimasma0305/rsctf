import { ActionIcon, Button, Group, Pagination, Text, useComputedColorScheme } from '@mantine/core'
import {
  mdiArrowDown,
  mdiArrowLeft,
  mdiArrowRight,
  mdiArrowUp,
  mdiCheck,
  mdiCircleOutline,
  mdiFormatListBulleted,
  mdiRestore,
  mdiViewGridOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { memo, useEffect, useId, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import classes from './Competition.module.css'
import { challengePage, projectHorizonNode, projectSpherePoint } from './model'
import { useGlobeRotation } from './useGlobeRotation'

export interface GlobeNode {
  id: string
  label: string
  count?: number
  solved?: boolean
  selected?: boolean
  onSelect: () => void
}

// Deterministic geometry, independent of challenge data and viewer identity.
const particles = Array.from({ length: 700 }, (_, index) => {
  const y = 1 - (index / 699) * 2
  const r = Math.sqrt(1 - y * y)
  const angle = index * Math.PI * (3 - Math.sqrt(5))
  return [Math.cos(angle) * r, y, Math.sin(angle) * r] as const
})

const GlobeSurface = memo(({ yaw, pitch }: { yaw: number; pitch: number }) => {
  const canvas = useRef<HTMLCanvasElement>(null)
  const scheme = useComputedColorScheme('dark')
  useEffect(() => {
    const element = canvas.current
    if (!element) return
    const context = element.getContext('2d')
    if (!context) return
    // Fixed, bounded bitmap. No perpetual animation, timers, network or WebGL context.
    const size = 800
    const radius = 348
    // Keep the larger stage crisp with one bounded, 1200px bitmap at every viewport.
    element.width = size * 1.5
    element.height = size * 1.5
    context.scale(1.5, 1.5)
    context.clearRect(0, 0, size, size)
    const dark = scheme === 'dark'
    const color = dark ? '115,166,225' : '40,87,147'
    const gradient = context.createRadialGradient(325, 280, 0, 400, 400, radius)
    gradient.addColorStop(0, dark ? '#15273d' : '#eaf2fc')
    gradient.addColorStop(0.85, dark ? '#081522' : '#dce8f6')
    gradient.addColorStop(0.985, dark ? '#142b45' : '#b3cbe9')
    gradient.addColorStop(1, dark ? '#6cafd0' : '#85add4')
    context.fillStyle = gradient
    context.beginPath()
    context.arc(400, 400, radius, 0, Math.PI * 2)
    context.fill()
    const drawLine = (points: readonly (readonly [number, number, number])[]) => {
      context.beginPath()
      let connected = false
      points.forEach(([x, y, z]) => {
        const point = projectSpherePoint(x, y, z, yaw, pitch)
        if (point.z < 0) {
          connected = false
          return
        }
        const px = 400 + point.x * radius
        const py = 400 + point.y * radius
        if (!connected) context.moveTo(px, py)
        else context.lineTo(px, py)
        connected = true
      })
      context.stroke()
    }
    context.strokeStyle = `rgba(${color},${dark ? 0.2 : 0.26})`
    context.lineWidth = 0.8
    for (let ring = 0; ring < 18; ring++) {
      const longitude = (ring * Math.PI) / 18
      drawLine(
        Array.from({ length: 97 }, (_, i) => {
          const a = (i * Math.PI * 2) / 96
          return [Math.cos(a) * Math.cos(longitude), Math.sin(a), Math.cos(a) * Math.sin(longitude)] as const
        })
      )
    }
    for (let ring = -4; ring <= 4; ring++) {
      const y = ring / 5
      const r = Math.sqrt(1 - y * y)
      drawLine(
        Array.from({ length: 97 }, (_, i) => {
          const a = (i * Math.PI * 2) / 96
          return [Math.cos(a) * r, y, Math.sin(a) * r] as const
        })
      )
    }
    for (const [x, y, z] of particles) {
      const point = projectSpherePoint(x, y, z, yaw, pitch)
      if (point.z < 0) continue
      context.fillStyle = `rgba(${color},0.72)`
      context.beginPath()
      context.arc(400 + point.x * radius, 400 + point.y * radius, 1.5, 0, Math.PI * 2)
      context.fill()
    }
  }, [yaw, pitch, scheme])
  return <canvas ref={canvas} className={classes.sphere} aria-hidden="true" />
})

export const ChallengeGlobe = memo(
  ({ nodes, scope, onList }: { nodes: GlobeNode[]; scope: string; onList: () => void }) => {
    const { t } = useTranslation()
    const [page, setPage] = useState(1)
    const { yaw, pitch, rotate, reset, focus, stageProps } = useGlobeRotation(scope)
    const [focusedTarget, setFocusedTarget] = useState<{ scope: string; id: string } | null>(null)
    const focusedId = focusedTarget?.scope === scope ? focusedTarget.id : null
    const controlsHint = useId()
    const pageData = useMemo(() => challengePage(nodes, page, 8), [nodes, page])
    const focusedIndex = pageData.items.findIndex((node) => node.id === focusedId)
    useEffect(() => {
      setPage(1)
      setFocusedTarget(null)
    }, [scope])
    useEffect(() => {
      if (!focusedId) return
      if (focusedIndex < 0) {
        setFocusedTarget(null)
        reset()
      } else {
        focus(focusedIndex)
      }
    }, [focusedTarget, focusedId, focusedIndex, focus, reset])

    const selectTarget = (node: GlobeNode) => {
      setFocusedTarget({ scope, id: node.id })
      // Selection stays with the parent; opening details never waits for the camera.
      node.onSelect()
    }

    return (
      <section
        className={classes.globe}
        aria-label={t('game.arena.globe', 'Challenge globe')}
        data-challenge-globe
        data-globe-focus={focusedId ?? undefined}
        data-motion="page"
      >
        <Group justify="space-between" gap="xs">
          <Text component="h2" size="sm" fw={650} m={0}>
            {t('game.arena.globe', 'Challenge globe')}
          </Text>
          <Text size="xs" c="dimmed">
            {t('game.arena.globe_hint', 'Drag in any direction to rotate 360°')}
          </Text>
        </Group>
        <div className={classes.globeExplorer}>
          <div
            {...stageProps}
            className={classes.globeStage}
            data-globe-stage
            data-globe-yaw={yaw}
            data-globe-pitch={pitch}
            role="group"
            tabIndex={0}
            aria-label={t('game.arena.globe_navigation', 'Globe navigation')}
            aria-describedby={controlsHint}
          >
            <span id={controlsHint} className={classes.srOnly}>
              {t(
                'game.arena.globe_controls',
                'Drag or swipe in any direction to rotate. Use all four arrow keys to rotate, and Home to reset. Swipe outside the globe to scroll the page.'
              )}
            </span>
            <GlobeSurface yaw={yaw} pitch={pitch} />
            <div className={classes.nodes}>
              {pageData.items.map((node, index) => {
                const point = projectHorizonNode(index, yaw, pitch)
                return (
                  <button
                    key={node.id}
                    type="button"
                    className={classes.node}
                    data-selected={node.selected || undefined}
                    data-focused={node.id === focusedId || undefined}
                    data-centered={(Math.abs(point.x - 50) < 0.5 && Math.abs(point.y - 29.36) < 0.5) || undefined}
                    data-solved={node.solved || undefined}
                    data-globe-node={node.id}
                    aria-pressed={node.selected ?? false}
                    hidden={!point.visible}
                    title={node.label}
                    onClick={() => selectTarget(node)}
                    style={
                      {
                        '--node-x': `${point.x}%`,
                        '--node-y': `${point.y}%`,
                      } as React.CSSProperties
                    }
                  >
                    <span className={classes.nodeDot} aria-hidden="true">
                      <Icon
                        path={node.count !== undefined ? mdiViewGridOutline : node.solved ? mdiCheck : mdiCircleOutline}
                        size={0.8}
                      />
                    </span>
                    <span className={classes.nodeLabel}>
                      {node.label}
                      {node.count !== undefined && <span> · {node.count}</span>}
                    </span>
                    {node.solved && <span className={classes.srOnly}>{t('common.workspace.solved', 'Solved')}</span>}
                  </button>
                )
              })}
            </div>
          </div>
          <nav className={classes.globeNavigator} aria-label={t('game.arena.globe_navigation', 'Globe navigation')}>
            <Text size="xs" c="dimmed" className={classes.navigatorTitle}>
              {t(
                'game.arena.explore_help',
                'Select a category to explore, or a challenge to focus the globe and open its details.'
              )}
            </Text>
            <div className={classes.globeChoices}>
              {pageData.items.map((node, index) => (
                <button
                  key={node.id}
                  type="button"
                  className={classes.globeChoice}
                  data-selected={node.selected || undefined}
                  data-focused={node.id === focusedId || undefined}
                  data-globe-choice={node.id}
                  data-guide={node.count !== undefined ? 'challenge-category' : 'challenge-card'}
                  aria-label={t('game.arena.select_target', { defaultValue: 'Select {{target}}', target: node.label })}
                  aria-pressed={node.selected ?? false}
                  onClick={() => selectTarget(node)}
                >
                  <span className={classes.choiceIndex} aria-hidden="true">
                    {node.solved ? (
                      <Icon path={mdiCheck} size={0.8} />
                    ) : (
                      String((pageData.current - 1) * 8 + index + 1).padStart(2, '0')
                    )}
                  </span>
                  <span className={classes.choiceLabel}>{node.label}</span>
                  {node.solved && <span className={classes.srOnly}>{t('common.workspace.solved', 'Solved')}</span>}
                  {node.count !== undefined && <span className={classes.choiceCount}>{node.count}</span>}
                  <Icon path={mdiArrowRight} size={0.75} aria-hidden="true" />
                </button>
              ))}
            </div>
          </nav>
        </div>
        <Group justify="space-between" gap="xs" className={classes.globeControls}>
          <Group gap="sm" className={classes.legend}>
            <span>
              <Icon path={mdiCircleOutline} size={0.65} />
              {t('game.arena.available', 'Available')}
            </span>
            <span>
              <Icon path={mdiCheck} size={0.65} />
              {t('common.workspace.solved', 'Solved')}
            </span>
          </Group>
          <Group gap={4}>
            <ActionIcon
              size="lg"
              variant="default"
              aria-label={t('game.arena.rotate_up', 'Rotate globe up')}
              onClick={() => rotate(0, Math.PI / 12)}
            >
              <Icon path={mdiArrowUp} size={0.8} />
            </ActionIcon>
            <ActionIcon
              size="lg"
              variant="default"
              aria-label={t('game.arena.rotate_left', 'Rotate globe left')}
              onClick={() => rotate(-Math.PI / 12)}
            >
              <Icon path={mdiArrowLeft} size={0.8} />
            </ActionIcon>
            <ActionIcon
              size="lg"
              variant="default"
              aria-label={t('game.arena.reset_view', 'Reset globe view')}
              onClick={reset}
            >
              <Icon path={mdiRestore} size={0.8} />
            </ActionIcon>
            <ActionIcon
              size="lg"
              variant="default"
              aria-label={t('game.arena.rotate_right', 'Rotate globe right')}
              onClick={() => rotate(Math.PI / 12)}
            >
              <Icon path={mdiArrowRight} size={0.8} />
            </ActionIcon>
            <ActionIcon
              size="lg"
              variant="default"
              aria-label={t('game.arena.rotate_down', 'Rotate globe down')}
              onClick={() => rotate(0, -Math.PI / 12)}
            >
              <Icon path={mdiArrowDown} size={0.8} />
            </ActionIcon>
          </Group>
        </Group>
        <Group justify="space-between" gap="xs" mt="xs">
          {pageData.pages > 1 && (
            <Pagination
              autoContrast
              getControlProps={(control) => ({ 'aria-label': t(`common.pagination.${control}`) })}
              getItemProps={(page) => ({ 'aria-label': t('common.pagination.page', { page }) })}
              size="sm"
              total={pageData.pages}
              value={pageData.current}
              onChange={(next) => {
                setFocusedTarget(null)
                reset()
                setPage(next)
              }}
              siblings={0}
              boundaries={1}
            />
          )}
          <Button
            variant="subtle"
            size="compact-xs"
            leftSection={<Icon path={mdiFormatListBulleted} size={0.7} />}
            onClick={onList}
          >
            {t('game.arena.list', 'List')}
          </Button>
        </Group>
      </section>
    )
  }
)
