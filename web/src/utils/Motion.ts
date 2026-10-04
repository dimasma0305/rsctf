import type { TransitionProps } from '@mantine/core'

export const MOTION_EASE = 'cubic-bezier(0.2, 0, 0, 1)'

// Small, finite entrances. Mantine owns interruption, exit cleanup and reduced
// motion; route identity and request lifecycles never wait for an animation.
export const DIALOG_TRANSITION = {
  transition: {
    in: { opacity: 1, transform: 'translateY(0)' },
    out: { opacity: 0, transform: 'translateY(8px)' },
    common: {},
    transitionProperty: 'opacity, transform',
  },
  duration: 180,
  exitDuration: 140,
  timingFunction: MOTION_EASE,
} satisfies Partial<TransitionProps>

export const DRAWER_TRANSITION = {
  duration: 220,
  exitDuration: 160,
  timingFunction: MOTION_EASE,
} satisfies Partial<TransitionProps>

export const POPOVER_TRANSITION = {
  transition: 'fade',
  duration: 140,
  exitDuration: 100,
  timingFunction: MOTION_EASE,
} satisfies Partial<TransitionProps>
