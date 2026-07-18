import { Box, Divider, Group, Stack, Text } from '@mantine/core'
import { FC } from 'react'
import { Copyright } from '@Components/Copyright'
import { FooterRender } from '@Components/FooterRender'
import { MainIcon } from '@Components/icon/MainIcon'
import { useIsMobile } from '@Utils/ThemeOverride'
import { useConfig } from '@Hooks/useConfig'
import classes from '@Styles/AppFooter.module.css'
import logoClasses from '@Styles/LogoHeader.module.css'

export const AppFooter: FC = () => {
  const { config } = useConfig()
  const isMobile = useIsMobile()

  return (
    <Box component="footer" className={classes.wrapper}>
      <Stack gap="md" className={classes.inner}>
        <Group justify="space-between" align="center" gap="lg" wrap="wrap">
          <Group gap="sm" wrap="nowrap">
            <MainIcon size={isMobile ? '2rem' : '2.4rem'} />
            <Stack gap={0}>
              <Text fw={750} size="lg">
                RS<span className={logoClasses.brand}>::</span>CTF
              </Text>
              <Text size="xs" c="dimmed">
                {config?.slogan?.trim() || 'Capture. Compete. Conquer.'}
              </Text>
            </Stack>
          </Group>
          <Copyright isMobile={isMobile} />
        </Group>
        {config.footerInfo && (
          <>
            <Divider />
            <FooterRender source={config.footerInfo} />
          </>
        )}
      </Stack>
    </Box>
  )
}
