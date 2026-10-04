import { Avatar, Badge, Button, Card, Group, Stack, Text, Title, Tooltip } from '@mantine/core'
import { mdiLockOutline, mdiCrown, mdiArrowRight } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC } from 'react'
import { useTranslation } from 'react-i18next'
import { TeamInfoModel } from '@Api'
import classes from '@Styles/TeamCard.module.css'

interface TeamCardProps {
  team: TeamInfoModel
  isCaptain: boolean
  onEdit: () => void
}

export const TeamCard: FC<TeamCardProps> = ({ team, isCaptain, onEdit }) => {
  const { t } = useTranslation()
  const name = team.name ?? t('team.label.name', 'Team')
  const action = isCaptain
    ? t('common.workspace.manage_team', 'Manage team')
    : t('common.workspace.view_team', 'View team')
  return (
    <Card component="article" withBorder radius="lg" className={classes.card}>
      <Group wrap="nowrap" align="flex-start">
        <Avatar imageProps={{ loading: 'lazy' }} alt="" size={48} radius="md" src={team.avatar}>
          {name.slice(0, 1)}
        </Avatar>
        <Stack gap={5} miw={0} style={{ flex: 1 }}>
          <Title order={2} size="h4" className={classes.name}>
            {name}
          </Title>
          <Group gap={5}>
            {isCaptain && (
              <Badge
                variant="light"
                color="yellow"
                leftSection={<Icon path={mdiCrown} size={0.65} aria-hidden="true" />}
              >
                {t('team.content.role.captain', 'Captain')}
              </Badge>
            )}
            {team.locked && (
              <Badge
                variant="light"
                color="gray"
                leftSection={<Icon path={mdiLockOutline} size={0.65} aria-hidden="true" />}
              >
                {t('team.label.locked', 'Locked')}
              </Badge>
            )}
          </Group>
        </Stack>
      </Group>
      <Text size="sm" c="dimmed" lineClamp={2}>
        {team.bio || t('team.placeholder.bio')}
      </Text>
      <Group justify="space-between" wrap="wrap">
        <Text size="xs" c="dimmed">
          {t('team.label.members')} · {team.members?.length ?? 0}
        </Text>
        <Avatar.Group className={classes.avatarGroup}>
          {team.members?.slice(0, 6).map((member) => (
            <Tooltip key={member.id} label={member.userName}>
              <Avatar
                imageProps={{ loading: 'lazy' }}
                alt={member.userName ?? ''}
                size={30}
                radius="xl"
                src={member.avatar}
              >
                {member.userName?.slice(0, 1)}
              </Avatar>
            </Tooltip>
          ))}
          {(team.members?.length ?? 0) > 6 && (
            <Avatar size={30} radius="xl">
              +{team.members!.length - 6}
            </Avatar>
          )}
        </Avatar.Group>
      </Group>
      <Button
        variant="default"
        onClick={onEdit}
        aria-label={`${action}: ${name}`}
        rightSection={<Icon path={mdiArrowRight} size={0.75} aria-hidden="true" />}
      >
        {action}
      </Button>
    </Card>
  )
}
