import { BoxProps, Center, Group, MantineColor, em, useMantineColorScheme, useMantineTheme } from '@mantine/core'
import cx from 'clsx'
import { FC } from 'react'
import classes from '@Styles/GameProgress.module.css'

export interface GameProgressProps extends BoxProps {
  thickness?: number
  spikeLength?: number
  percentage: number
  color?: MantineColor
  active?: boolean
  ariaLabel?: string
}

export const GameProgress: FC<GameProgressProps> = (props: GameProgressProps) => {
  const {
    thickness = 4,
    spikeLength = 250,
    percentage,
    color,
    active = false,
    ariaLabel = 'Game progress',
    ...others
  } = props

  const theme = useMantineTheme()
  const { colorScheme } = useMantineColorScheme()

  const normalizedPercentage = Math.min(100, Math.max(0, percentage))
  const roundedPercentage = Math.round(normalizedPercentage)
  const pulsing = active && normalizedPercentage < 100
  const resolvedColor = active ? (colorScheme === 'dark' ? 'light' : (color ?? theme.primaryColor)) : 'gray'
  const spikeColor = theme.colors[resolvedColor][5]
  const bgColor = theme.colors[resolvedColor][2]

  return (
    <Center
      role="progressbar"
      aria-label={ariaLabel}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={roundedPercentage}
      aria-valuetext={`${roundedPercentage}%`}
      data-active={active || undefined}
      py={(thickness * spikeLength) / 100}
      {...others}
      __vars={{
        '--thickness': em(thickness),
        '--spike-length': `${spikeLength}%`,
        '--neg-spike-length': `${-spikeLength}%`,
        '--percentage': `${normalizedPercentage}%`,
        '--spike-color': spikeColor,
        '--bg-color': bgColor,
        '--pulsing-display': pulsing ? 'block' : 'none',
      }}
    >
      <div className={classes.back} aria-hidden="true">
        <Group justify="right" className={classes.box}>
          <div className={classes.bar}>
            <div />
          </div>
          <div className={classes.spikes}>
            <div className={cx(classes.spike, classes.r)} />
            <div className={cx(classes.spike, classes.l)} />
            <div className={cx(classes.spike, classes.t)} />
            <div className={cx(classes.spike, classes.b)} />
          </div>
        </Group>
      </div>
    </Center>
  )
}
