import { StyleProp, rem } from '@mantine/core'
import { FC, SVGProps } from 'react'
import classes from '@Styles/Icon.module.css'

export interface MainIconProps {
  ignoreTheme?: boolean
  size?: StyleProp<React.CSSProperties['width']>
}

export const MainIcon: FC<MainIconProps & SVGProps<SVGSVGElement>> = ({ ignoreTheme, size, ...svgProps }) => {
  return (
    <svg
      width="480"
      height="480"
      viewBox="0 0 4800 4800"
      style={{
        marginLeft: `calc(${rem(size)} / 10)`,
        width: rem(size) || 'auto',
        height: 'auto',
        aspectRatio: '1 / 1',
      }}
      {...svgProps}
    >
      <g fill="none" strokeLinecap="round" strokeLinejoin="round" strokeWidth="520">
        <path
          className={ignoreTheme ? undefined : classes.mainStroke}
          stroke={ignoreTheme ? '#fff' : undefined}
          d="M900 3980V840h720c570 0 900 285 900 730 0 450-330 730-900 730h-540l1440 1680"
        />
        <path
          className={classes.frontStroke}
          d="M2520 3980c430 260 1070 190 1400-190 330-390 150-930-330-1140l-560-245c-430-190-570-560-370-930 210-390 690-560 1120-415 200 65 360 170 480 315"
        />
      </g>
    </svg>
  )
}
