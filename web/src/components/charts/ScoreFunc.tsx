import { useMantineColorScheme, useMantineTheme } from '@mantine/core'
import type { EChartsOption } from 'echarts'
import { FC, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { ScoreCurve } from '@Api'
import { EchartsContainer } from '@Components/charts/EchartsContainer'

interface ScoreFuncProps {
  originalScore: number
  difficulty: number
  minScoreRate: number
  currentAcceptCount: number
  curve?: ScoreCurve
}

export const ScoreFunc: FC<ScoreFuncProps> = ({
  originalScore,
  difficulty,
  minScoreRate,
  currentAcceptCount,
  curve = ScoreCurve.Standard,
}) => {
  const toX = (x: number) => (x * 6 * difficulty) / 100
  // Mirrors GameChallenge.CalculateChallengeScore exactly so the preview matches the
  // real score. Keep these three branches in sync with the backend's ScoreCurve switch.
  const func = (x: number) => {
    if (x <= 1) return originalScore
    let factor: number
    switch (curve) {
      case ScoreCurve.Linear:
        factor = Math.max(minScoreRate, 1 - (1 - minScoreRate) * ((x - 1) / difficulty))
        break
      case ScoreCurve.Logarithmic:
        factor = minScoreRate + (1 - minScoreRate) / (1 + Math.log(x) / difficulty)
        break
      default:
        factor = minScoreRate + (1 - minScoreRate) * Math.exp((1 - x) / difficulty)
    }
    return Math.floor(originalScore * factor)
  }

  const curScore = func(currentAcceptCount)
  const showCount = currentAcceptCount > 5.8 * difficulty ? 5.8 * difficulty : currentAcceptCount
  const theme = useMantineTheme()
  const plotData = [...Array(100).keys()].map((x) => [toX(x), func(toX(x))])
  const { colorScheme } = useMantineColorScheme()
  const { t } = useTranslation()
  const primaryColors = theme.colors[theme.primaryColor]
  const color = primaryColors[colorScheme === 'dark' ? 8 : 6]

  const option: EChartsOption = useMemo(
    () =>
      ({
        animation: false,
        backgroundColor: 'transparent',
        grid: {
          top: 30,
          left: 40,
          right: 70,
          bottom: 30,
          backgroundColor: 'transparent',
        },
        xAxis: {
          name: t('admin.content.games.challenges.solve_count'),
        },
        yAxis: {
          name: t('admin.content.games.challenges.score'),
          min: 0,
          max: Math.ceil((originalScore * 1.2) / 100) * 100,
        },
        series: [
          {
            type: 'line',
            showSymbol: false,
            clip: true,
            color: color,
            data: plotData,
            markPoint: {
              label: {
                show: true,
                fontSize: 10,
                formatter: '{c}',
              },
              symbol: 'pin',
              symbolSize: 40,
              symbolOffset: [0, 0],
              data: [
                {
                  name: t('game.label.score'),
                  value: curScore,
                  xAxis: showCount,
                  yAxis: curScore,
                },
              ],
            },
            markLine: {
              symbol: 'none',
              data: [
                {
                  yAxis: Math.floor(originalScore * minScoreRate),
                  label: {
                    position: 'end',
                    formatter: 'min: {c}',
                  },
                },
              ],
            },
          },
        ],
      }) satisfies EChartsOption,
    [theme, originalScore, difficulty, minScoreRate, currentAcceptCount, curve]
  )

  return (
    <EchartsContainer
      option={option}
      opts={{
        renderer: 'svg',
      }}
      style={{
        width: '100%',
        height: '100%',
        display: 'flex',
      }}
    />
  )
}
