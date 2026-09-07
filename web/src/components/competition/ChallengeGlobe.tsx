import { ActionIcon, Button, Group, Pagination, Text, useComputedColorScheme } from '@mantine/core'
import {
  mdiArrowLeft,
  mdiArrowRight,
  mdiCheck,
  mdiCircleOutline,
  mdiFormatListBulleted,
  mdiRestore,
  mdiViewGridOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { memo, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import classes from './Competition.module.css'
import { challengePage, projectSpherePoint } from './model'

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

const GlobeSurface = memo(({ yaw }: { yaw: number }) => {
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
    element.width = size
    element.height = size
    context.clearRect(0, 0, size, size)
    const dark = scheme === 'dark'
    const color = dark ? '115,166,225' : '40,87,147'
    const gradient = context.createRadialGradient(325, 280, 0, 400, 400, radius)
    gradient.addColorStop(0, dark ? '#15273d' : '#eaf2fc')
    gradient.addColorStop(0.85, dark ? '#081522' : '#dce8f6')
    gradient.addColorStop(1, dark ? '#142b45' : '#b3cbe9')
    context.fillStyle = gradient
    context.beginPath()
    context.arc(400, 400, radius, 0, Math.PI * 2)
    context.fill()
    const drawLine = (points: readonly (readonly [number, number, number])[]) => {
      context.beginPath()
      points.forEach(([x, y, z], i) => {
        const point = projectSpherePoint(x, y, z, yaw, -0.22)
        const px = 400 + point.x * radius
        const py = 400 + point.y * radius
        if (i === 0) context.moveTo(px, py)
        else context.lineTo(px, py)
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
      const point = projectSpherePoint(x, y, z, yaw, -0.22)
      context.fillStyle = `rgba(${color},${point.z > 0 ? 0.72 : 0.17})`
      context.beginPath()
      context.arc(400 + point.x * radius, 400 + point.y * radius, point.z > 0 ? 1.5 : 0.9, 0, Math.PI * 2)
      context.fill()
    }
  }, [yaw, scheme])
  return <canvas ref={canvas} className={classes.sphere} aria-hidden="true" />
})

export const ChallengeGlobe = memo(
  ({ nodes, scope, onList }: { nodes: GlobeNode[]; scope: string; onList: () => void }) => {
    const { t } = useTranslation()
    const [page, setPage] = useState(1)
    const [yaw, setYaw] = useState(0)
    const drag = useRef<{ id: number; x: number; yaw: number } | null>(null)
    const pendingFrame = useRef<number | null>(null)
    const pageData = useMemo(() => challengePage(nodes, page, 8), [nodes, page])
    useEffect(() => {
      setPage(1)
      setYaw(0)
    }, [scope])
    useEffect(
      () => () => {
        if (pendingFrame.current !== null) cancelAnimationFrame(pendingFrame.current)
      },
      []
    )

    return (
      <section className={classes.globe} aria-label={t('game.arena.globe', 'Challenge globe')} data-challenge-globe>
        <div
          className={classes.globeStage}
          onPointerDown={(event) => {
            if ((event.target as HTMLElement).closest('button')) return
            if (event.pointerType !== 'mouse') return // Preserve normal touch scrolling; use the rotation buttons.
            drag.current = { id: event.pointerId, x: event.clientX, yaw }
            event.currentTarget.setPointerCapture(event.pointerId)
          }}
          onPointerMove={(event) => {
            if (!drag.current || event.pointerId !== drag.current.id || pendingFrame.current !== null) return
            const next = drag.current.yaw + (event.clientX - drag.current.x) / 180
            pendingFrame.current = requestAnimationFrame(() => {
              setYaw(next)
              pendingFrame.current = null
            })
          }}
          onPointerUp={() => {
            drag.current = null
          }}
          onPointerCancel={() => {
            drag.current = null
          }}
          onLostPointerCapture={() => {
            drag.current = null
          }}
        >
          <GlobeSurface yaw={yaw} />
          <div className={classes.nodes}>
            {pageData.items.map((node, index) => {
              const x = index % 2 === 0 ? -0.56 : 0.56
              const y = -0.64 + Math.floor(index / 2) * 0.42
              const point = projectSpherePoint(x, y, Math.sqrt(Math.max(0, 1 - x * x - y * y)), yaw, 0)
              return (
                <button
                  key={node.id}
                  type="button"
                  className={classes.node}
                  data-selected={node.selected || undefined}
                  data-solved={node.solved || undefined}
                  data-globe-node={node.id}
                  aria-pressed={node.selected ?? false}
                  hidden={point.z < 0}
                  onClick={node.onSelect}
                  style={
                    { '--node-x': `${50 + point.x * 36}%`, '--node-y': `${50 + point.y * 43}%` } as React.CSSProperties
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
        <Group justify="space-between" gap="xs" className={classes.globeControls}>
          <Text size="xs" c="dimmed">
            {t('game.arena.explore_help', 'Select a category or challenge to explore.')}
          </Text>
          <Group gap={4}>
            <ActionIcon
              size="lg"
              variant="default"
              aria-label={t('game.arena.rotate_left', 'Rotate globe left')}
              onClick={() => setYaw(yaw - 0.3)}
            >
              <Icon path={mdiArrowLeft} size={0.8} />
            </ActionIcon>
            <ActionIcon
              size="lg"
              variant="default"
              aria-label={t('game.arena.reset_view', 'Reset globe view')}
              onClick={() => setYaw(0)}
            >
              <Icon path={mdiRestore} size={0.8} />
            </ActionIcon>
            <ActionIcon
              size="lg"
              variant="default"
              aria-label={t('game.arena.rotate_right', 'Rotate globe right')}
              onClick={() => setYaw(yaw + 0.3)}
            >
              <Icon path={mdiArrowRight} size={0.8} />
            </ActionIcon>
          </Group>
        </Group>
        <Group justify="space-between" gap="xs" mt="xs">
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
          {pageData.pages > 1 && (
            <Pagination
              autoContrast
              getControlProps={(control) => ({ 'aria-label': t(`common.pagination.${control}`) })}
              getItemProps={(page) => ({ 'aria-label': t('common.pagination.page', { page }) })}
              size="sm"
              total={pageData.pages}
              value={pageData.current}
              onChange={setPage}
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
