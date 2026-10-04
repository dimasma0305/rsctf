import { Stack, Group, Button, Select } from '@mantine/core'
import { mdiArrowLeft, mdiArrowRight } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useParams, useLocation, useNavigate } from 'react-router'
import { WithGameEditTab, GameEditTabProps } from '@Components/admin/WithGameEditTab'
import { useEditChallenges } from '@Hooks/useEdit'
import classes from '@Styles/AdminTabs.module.css'

export const WithChallengeEdit: FC<GameEditTabProps> = ({ children, isLoading, ...rest }) => {
  const { pathname } = useLocation()
  const navigate = useNavigate()
  const { id, chalId } = useParams()
  const numId = Number(id),
    numCId = Number(chalId)
  const { challenges } = useEditChallenges(numId)
  const { t } = useTranslation()
  const index = challenges?.findIndex((challenge) => challenge.id === numCId) ?? -1
  const prev = index > 0 ? challenges?.[index - 1] : undefined
  const next = index >= 0 ? challenges?.[index + 1] : undefined
  const restpath = pathname.replace(/^\/admin\/games\/\d+\/challenges\/\d+/, '')
  const destination = (challengeId: number) => `/admin/games/${numId}/challenges/${challengeId}${restpath}`

  return (
    <WithGameEditTab isLoading={isLoading} {...rest}>
      <Stack gap="md">
        <Group align="end" gap="sm" data-challenge-switcher>
          <Select
            className={classes.challengeSwitcher}
            label={t('admin.navigation.switch_challenge')}
            searchable
            allowDeselect={false}
            value={String(numCId)}
            disabled={!challenges}
            data={(challenges ?? []).map((challenge) => ({
              value: String(challenge.id),
              label: `#${challenge.id} · ${challenge.title}`,
            }))}
            onChange={(value) => value && navigate(destination(Number(value)))}
          />
          <Group gap="xs">
            <Button
              variant="default"
              component={Link}
              disabled={isLoading || !prev?.id}
              aria-disabled={isLoading || !prev?.id || undefined}
              tabIndex={isLoading || !prev?.id ? -1 : undefined}
              onClick={(event) => {
                if (isLoading || !prev?.id) event.preventDefault()
              }}
              leftSection={<Icon path={mdiArrowLeft} size={0.85} />}
              to={prev?.id ? destination(prev.id) : '#'}
            >
              {t('admin.button.challenges.previous')}
            </Button>
            <Button
              variant="default"
              component={Link}
              disabled={isLoading || !next?.id}
              aria-disabled={isLoading || !next?.id || undefined}
              tabIndex={isLoading || !next?.id ? -1 : undefined}
              onClick={(event) => {
                if (isLoading || !next?.id) event.preventDefault()
              }}
              rightSection={<Icon path={mdiArrowRight} size={0.85} />}
              to={next?.id ? destination(next.id) : '#'}
            >
              {t('admin.button.challenges.next')}
            </Button>
          </Group>
        </Group>
        {children}
      </Stack>
    </WithGameEditTab>
  )
}
