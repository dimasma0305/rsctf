import {
  ActionIcon,
  Avatar,
  Badge,
  Box,
  Button,
  Center,
  Grid,
  Group,
  Image,
  Modal,
  ModalProps,
  PasswordInput,
  ScrollArea,
  Stack,
  Text,
  Textarea,
  TextInput,
  Tooltip,
  VisuallyHidden,
} from '@mantine/core'
import { Dropzone } from '@mantine/dropzone'
import { useClipboard } from '@mantine/hooks'
import { useModals } from '@mantine/modals'
import { notifications, showNotification, updateNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose, mdiContentCopy, mdiLinkVariant, mdiLockOutline, mdiRefresh, mdiStar } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useCallback, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { KeyedMutator } from 'swr'
import { ScrollingText } from '@Components/ScrollingText'
import { showErrorMsg, tryGetErrorMsg } from '@Utils/Shared'
import { IMAGE_MIME_TYPES } from '@Utils/Shared'
import api, { TeamInfoModel, TeamUserInfoModel } from '@Api'
import misc from '@Styles/Misc.module.css'
import styles from '@Styles/TeamEditModal.module.css'

interface TeamEditModalProps extends ModalProps {
  team: TeamInfoModel | null
  isCaptain: boolean
  teams?: TeamInfoModel[]
  mutateTeams: KeyedMutator<TeamInfoModel[]>
}

interface TeamMemberInfoProps {
  user: TeamUserInfoModel
  isCaptain: boolean
  onTransferCaptain: (user: TeamUserInfoModel) => void
  onKick: (user: TeamUserInfoModel) => void
}

const TeamMemberInfo: FC<TeamMemberInfoProps> = (props) => {
  const { user, isCaptain, onKick, onTransferCaptain } = props
  const [showBtns, setShowBtns] = useState(false)

  const { t } = useTranslation()

  return (
    <Group
      justify="space-between"
      gap={2}
      p="xs"
      className={styles.teamMember}
      onMouseEnter={() => setShowBtns(true)}
      onMouseLeave={() => setShowBtns(false)}
      onClick={() => setShowBtns(!showBtns)}
    >
      <Group justify="left">
        <Avatar alt="avatar" src={user.avatar} radius="xl" size="md">
          {user.userName?.slice(0, 1) ?? 'U'}
        </Avatar>
        <ScrollingText text={user.userName ?? ''} fw={500} size="sm" maw={220} />
      </Group>
      {isCaptain && showBtns && (
        <Group gap="xs" justify="right">
          <Tooltip label={t('team.label.transfer')}>
            <ActionIcon
              variant="light"
              color="yellow"
              aria-label={t('team.label.transfer')}
              onClick={(e) => {
                e.stopPropagation()
                onTransferCaptain(user)
              }}
            >
              <Icon path={mdiStar} size={0.8} />
            </ActionIcon>
          </Tooltip>
          <Tooltip label={t('team.label.kick')}>
            <ActionIcon
              variant="light"
              color="red"
              aria-label={t('team.label.kick')}
              onClick={(e) => {
                e.stopPropagation()
                onKick(user)
              }}
            >
              <Icon path={mdiClose} size={0.8} />
            </ActionIcon>
          </Tooltip>
        </Group>
      )}
    </Group>
  )
}

export const TeamEditModal: FC<TeamEditModalProps> = (props) => {
  const { team, isCaptain, teams, mutateTeams, ...modalProps } = props

  const teamId = team?.id

  const [teamInfo, setTeamInfo] = useState<TeamInfoModel | null>(team)
  const [dropzoneOpened, setDropzoneOpened] = useState(false)
  const [avatarFile, setAvatarFile] = useState<File | null>(null)
  const [inviteCode, setInviteCode] = useState('')
  const [inviteRevision, setInviteRevision] = useState<number | null>(null)
  const [inviteLoading, setInviteLoading] = useState(false)
  const [inviteLoadError, setInviteLoadError] = useState(false)
  const inviteRequestGeneration = useRef(0)
  const inviteMutationOwner = useRef(false)
  const inviteOperationId = useRef<string | null>(null)
  const [disabled, setDisabled] = useState(false)
  const mutationOwner = useRef<AbortController | null>(null)
  const profileOperation = useRef<{ digest: string; id: string } | null>(null)
  const clipboard = useClipboard()
  const locked = teamInfo?.locked ?? false
  const captain = teamInfo?.members?.filter((x) => x.captain)[0]
  const crew = teamInfo?.members?.filter((x) => !x.captain)

  const modals = useModals()

  const { t } = useTranslation()
  const teamName = teamInfo?.name ?? team?.name ?? t('team.label.name', { defaultValue: 'Team' })
  const avatarModalTitle = t('team.content.avatar_upload.title', {
    defaultValue: 'Change avatar for {{team}}',
    team: teamName,
  })
  const avatarDropzonePrompt = t('common.content.drop_zone.content', {
    defaultValue: 'Drag and drop an image or click here to select {{type}}',
    type: t('common.content.drop_zone.type.avatar', { defaultValue: 'avatar' }),
  })
  const avatarDropzoneLimit = t('common.content.drop_zone.limit', {
    defaultValue: 'Please select an image less than 3MB',
  })

  useEffect(() => {
    setTeamInfo(team)
    profileOperation.current = null
    mutationOwner.current?.abort()
    mutationOwner.current = null
    setDisabled(false)
  }, [team])

  useEffect(
    () => () => {
      mutationOwner.current?.abort()
    },
    []
  )

  const loadInviteCode = useCallback(async () => {
    if (!isCaptain || !teamId || !props.opened) return
    const generation = ++inviteRequestGeneration.current
    setInviteLoading(true)
    setInviteLoadError(false)
    try {
      const response = await api.team.teamInviteCode(teamId)
      if (generation !== inviteRequestGeneration.current) return
      setInviteCode(response.data.code)
      setInviteRevision(response.data.revision)
    } catch {
      if (generation === inviteRequestGeneration.current) setInviteLoadError(true)
    } finally {
      if (generation === inviteRequestGeneration.current) setInviteLoading(false)
    }
  }, [isCaptain, props.opened, teamId])

  useEffect(() => {
    inviteRequestGeneration.current += 1
    setInviteCode('')
    setInviteRevision(null)
    setInviteLoadError(false)
    inviteOperationId.current = null
    if (props.opened) void loadInviteCode()
    return () => {
      inviteRequestGeneration.current += 1
    }
  }, [loadInviteCode, props.opened])

  const onConfirmLeaveTeam = async () => {
    if (!teamInfo || isCaptain) return

    try {
      await api.team.teamLeave(teamInfo.id!)
      showNotification({
        color: 'teal',
        title: t('team.notification.leave.success'),
        message: t('team.notification.updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutateTeams(
        teams?.filter((x) => x.id !== teamInfo.id),
        { revalidate: false }
      )
      setInviteCode('')
      setTeamInfo(null)
      props.onClose()
    } catch (e) {
      showErrorMsg(e, t)
    }
  }

  const onConfirmDisbandTeam = async () => {
    if (!teamInfo || !isCaptain) return

    try {
      await api.team.teamDeleteTeam(teamInfo.id!)
      showNotification({
        color: 'teal',
        title: t('team.notification.disband.success'),
        message: t('team.notification.updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      setInviteCode('')
      setTeamInfo(null)
      mutateTeams(
        teams?.filter((x) => x.id !== teamInfo.id),
        { revalidate: false }
      )
      props.onClose()
    } catch (e) {
      showErrorMsg(e, t)
    }
  }

  const onTransferCaptain = async (userId: string) => {
    if (!teamInfo || !isCaptain) return

    try {
      await api.team.teamTransfer(teamInfo.id!, {
        newCaptainId: userId,
      })
      showNotification({
        color: 'teal',
        title: t('team.notification.transfer.success'),
        message: t('team.notification.updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutateTeams(
        teams?.map((x) => (x.id === teamInfo.id ? teamInfo : x)),
        {
          revalidate: false,
        }
      )
    } catch (e) {
      showErrorMsg(e, t)
    }
  }

  const onConfirmKickUser = async (userId: string) => {
    if (!teamInfo?.id || !isCaptain) return

    try {
      await api.team.teamKickUser(teamInfo.id, userId)
      showNotification({
        color: 'teal',
        title: t('team.notification.kick.success'),
        message: t('team.notification.updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutateTeams(
        teams?.map((x) => (x.id === teamInfo.id ? teamInfo : x)),
        {
          revalidate: false,
        }
      )
    } catch (e) {
      showErrorMsg(e, t)
    }
  }

  const copyInviteCode = () => {
    if (!inviteCode) return
    clipboard.copy(inviteCode)
    showNotification({
      color: 'teal',
      message: t('team.notification.invite_code.copied'),
      icon: <Icon path={mdiCheck} size={1} />,
    })
  }

  const onRefreshInviteCode = async () => {
    if (!team?.id || inviteRevision == null || inviteMutationOwner.current) return

    inviteMutationOwner.current = true
    setInviteLoading(true)
    const operationId = inviteOperationId.current ?? crypto.randomUUID()
    inviteOperationId.current = operationId
    try {
      const response = await api.team.teamUpdateInviteToken(team.id, {
        operationId,
        expectedRevision: inviteRevision,
      })
      if (response.data.revision >= inviteRevision) {
        setInviteCode(response.data.code)
        setInviteRevision(response.data.revision)
      }
      inviteOperationId.current = null
      showNotification({
        color: 'teal',
        message: t('team.notification.invite_code.updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      showErrorMsg(e, t)
      setInviteLoadError(true)
    } finally {
      inviteMutationOwner.current = false
      setInviteLoading(false)
    }
  }

  const onChangeAvatar = async () => {
    if (!avatarFile || !teamInfo?.id || mutationOwner.current) return
    const owner = new AbortController()
    mutationOwner.current = owner
    setDisabled(true)
    notifications.clean()
    showNotification({
      id: 'upload-avatar',
      color: 'orange',
      message: t('common.avatar.uploading'),
      loading: true,
      autoClose: false,
    })

    try {
      const data = await api.team.teamAvatar(
        teamInfo.id,
        {
          file: avatarFile,
        },
        {
          signal: owner.signal,
        }
      )
      updateNotification({
        id: 'upload-avatar',
        color: 'teal',
        message: t('common.avatar.uploaded'),
        icon: <Icon path={mdiCheck} size={1} />,
        autoClose: true,
        loading: false,
      })
      setAvatarFile(null)
      const newTeamInfo = { ...teamInfo, avatar: data.data }
      setTeamInfo(newTeamInfo)
      mutateTeams(
        teams?.map((x) => (x.id === teamInfo.id ? newTeamInfo : x)),
        {
          revalidate: false,
        }
      )
    } catch (err) {
      updateNotification({
        id: 'upload-avatar',
        color: 'red',
        title: t('common.avatar.upload_failed'),
        message: tryGetErrorMsg(err, t),
        icon: <Icon path={mdiClose} size={1} />,
        autoClose: true,
        loading: false,
      })
    } finally {
      if (mutationOwner.current === owner) {
        mutationOwner.current = null
        setDisabled(false)
        setDropzoneOpened(false)
      }
    }
  }

  const onSaveChange = async () => {
    if (!teamInfo?.id || !team || mutationOwner.current) return
    const name = teamInfo.name?.trim() ?? ''
    const bio = teamInfo.bio ?? ''
    if (name === (team.name?.trim() ?? '') && bio === (team.bio ?? '')) return

    const profileRevision = teamInfo.profileRevision ?? 0
    const digest = JSON.stringify({ name, bio, profileRevision })
    if (profileOperation.current?.digest !== digest) {
      profileOperation.current = { digest, id: crypto.randomUUID() }
    }
    const operationId = profileOperation.current?.id
    if (!operationId) return
    const owner = new AbortController()
    mutationOwner.current = owner
    setDisabled(true)

    try {
      const response = await api.team.teamUpdateTeam(
        teamInfo.id,
        {
          name,
          bio,
          profileRevision,
          operationId,
        },
        { signal: owner.signal }
      )
      const saved = response.data
      setTeamInfo(saved)
      profileOperation.current = null
      showNotification({
        color: 'teal',
        message: t('team.notification.updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutateTeams(
        teams?.map((x) => (x.id === teamInfo.id ? saved : x)),
        {
          revalidate: false,
        }
      )
    } catch (e) {
      if (!owner.signal.aborted) showErrorMsg(e, t)
    } finally {
      if (mutationOwner.current === owner) {
        mutationOwner.current = null
        setDisabled(false)
      }
    }
  }

  return (
    <Modal
      {...modalProps}
      onClose={() => {
        if (disabled) return
        setDropzoneOpened(false)
        props.onClose()
      }}
    >
      <Stack gap="sm">
        {/* Team Info */}
        <Grid grow>
          <Grid.Col span={8}>
            {!isCaptain ? (
              <Stack gap={0} justify="center" h="100%">
                <Text size="sm" fw={500}>
                  {t('team.label.name')}
                </Text>
                <ScrollingText text={teamInfo?.name ?? 'team'} size="sm" maw={350} className={styles.readOnlyText} />
              </Stack>
            ) : (
              <TextInput
                label={t('team.label.name')}
                type="text"
                placeholder={team?.name ?? 'ctfteam'}
                w="100%"
                value={teamInfo?.name ?? 'team'}
                disabled={!isCaptain || locked || disabled}
                maxLength={128}
                onChange={(event) => setTeamInfo({ ...teamInfo, name: event.target.value })}
              />
            )}
          </Grid.Col>
          <Grid.Col span={4}>
            <Center>
              <Avatar
                alt={t('team.content.avatar_alt', {
                  defaultValue: '{{team}} avatar',
                  team: teamName,
                })}
                radius="xl"
                size={70}
                src={teamInfo?.avatar}
                role={isCaptain && !locked ? 'button' : undefined}
                tabIndex={isCaptain && !locked ? 0 : undefined}
                aria-label={isCaptain && !locked ? avatarModalTitle : undefined}
                style={isCaptain && !locked ? { cursor: 'pointer' } : undefined}
                onClick={() => isCaptain && !locked && !disabled && setDropzoneOpened(true)}
                onKeyDown={(event) => {
                  if (isCaptain && !locked && !disabled && (event.key === 'Enter' || event.key === ' ')) {
                    event.preventDefault()
                    setDropzoneOpened(true)
                  }
                }}
              >
                {teamInfo?.name?.slice(0, 1) ?? 'T'}
              </Avatar>
            </Center>
          </Grid.Col>
        </Grid>
        {locked && (
          <Group gap={6} c="orange">
            <Icon path={mdiLockOutline} size={0.8} />
            <Text size="sm" fw={500}>
              {t(
                'team.content.locked.note',
                'Your team is locked during an active game. Member and team changes are disabled.'
              )}
            </Text>
          </Group>
        )}
        {isCaptain && (
          <>
            <PasswordInput
              label={
                <Group gap={3}>
                  <Text fw={500} size="sm">
                    {t('team.label.invite_code')}
                  </Text>
                  <Tooltip label={t('team.label.refresh_code', 'Refresh invitation code')}>
                    <ActionIcon
                      size="sm"
                      aria-label={t('team.label.refresh_code', 'Refresh invitation code')}
                      loading={inviteLoading}
                      disabled={locked || inviteRevision == null || inviteMutationOwner.current}
                      onClick={onRefreshInviteCode}
                    >
                      <Icon path={mdiRefresh} size={1} />
                    </ActionIcon>
                  </Tooltip>
                  <Tooltip label={t('team.label.copy_code', 'Copy invitation code')}>
                    <ActionIcon
                      size="sm"
                      aria-label={t('team.label.copy_code', 'Copy invitation code')}
                      disabled={!inviteCode}
                      onClick={copyInviteCode}
                    >
                      <Icon path={mdiContentCopy} size={0.8} />
                    </ActionIcon>
                  </Tooltip>
                </Group>
              }
              value={inviteCode}
              placeholder="loading..."
              role="button"
              tabIndex={0}
              onClick={copyInviteCode}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault()
                  copyInviteCode()
                }
              }}
              readOnly
            />
            {inviteLoadError && (
              <Group justify="space-between" gap="xs" role="alert">
                <Text size="xs" c="red">
                  {t('team.error.invite_code_load', 'Invitation code could not be loaded. Retry safely.')}
                </Text>
                <Button
                  size="compact-xs"
                  variant="subtle"
                  loading={inviteLoading}
                  onClick={() => void loadInviteCode()}
                >
                  {t('common.button.retry', 'Retry')}
                </Button>
              </Group>
            )}
            <Button
              size="xs"
              variant="light"
              leftSection={<Icon path={mdiLinkVariant} size={0.8} />}
              disabled={!inviteCode}
              onClick={() => {
                const link = `${window.location.origin}/teams?join=${encodeURIComponent(inviteCode)}`
                clipboard.copy(link)
                showNotification({
                  color: 'teal',
                  message: t('team.notification.invite_code.link_copied'),
                  icon: <Icon path={mdiCheck} size={1} />,
                })
              }}
            >
              {t('team.button.copy_invite_link')}
            </Button>
          </>
        )}
        <Textarea
          label={t('team.label.bio')}
          placeholder={teamInfo?.bio ?? t('team.placeholder.bio')}
          value={teamInfo?.bio ?? ''}
          w="100%"
          disabled={!isCaptain || locked || disabled}
          maxLength={4096}
          autosize
          minRows={2}
          maxRows={4}
          onChange={(event) => setTeamInfo({ ...teamInfo, bio: event.target.value })}
        />
        <Text fw={500} size="sm">
          {t('team.label.members')}
        </Text>
        <ScrollArea h={210} offsetScrollbars>
          <Stack gap="xs">
            {captain && (
              <Group justify="space-between" p="xs" className={styles.captainGroup}>
                <Group justify="left">
                  <Avatar alt="avatar" src={captain.avatar} radius="xl" size="md">
                    {captain.userName?.slice(0, 1) ?? 'C'}
                  </Avatar>
                  <ScrollingText text={captain.userName ?? ''} fw={500} size="sm" maw={220} />
                </Group>
                <Badge color="orange" leftSection={<Icon path={mdiStar} size={0.6} />}>
                  {t('team.content.role.captain')}
                </Badge>
              </Group>
            )}
            {crew &&
              crew.map((user) => (
                <TeamMemberInfo
                  key={user.id}
                  isCaptain={isCaptain && !locked}
                  user={user}
                  onTransferCaptain={(user: TeamUserInfoModel) => {
                    modals.openConfirmModal({
                      title: t('team.content.transfer.confirm.title'),
                      children: (
                        <Text size="sm">
                          {t('team.content.transfer.confirm.message', {
                            team: teamInfo?.name,
                            user: user.userName,
                          })}
                        </Text>
                      ),
                      onConfirm: () => onTransferCaptain(user.id!),
                      confirmProps: { color: 'orange' },
                      zIndex: 10000,
                    })
                  }}
                  onKick={(user: TeamUserInfoModel) => {
                    modals.openConfirmModal({
                      title: t('team.content.kick.confirm.title'),
                      children: (
                        <Text size="sm">
                          {t('team.content.kick.confirm.message', {
                            user: user.userName,
                          })}
                        </Text>
                      ),
                      onConfirm: () => onConfirmKickUser(user.id!),
                      confirmProps: { color: 'orange' },
                      zIndex: 10000,
                    })
                  }}
                />
              ))}
          </Stack>
        </ScrollArea>

        <Group grow m="auto" w="100%">
          <Button
            fullWidth
            color="red"
            variant="outline"
            disabled={disabled || (isCaptain && locked)}
            onClick={() => {
              modals.openConfirmModal({
                title: isCaptain ? t('team.content.disband.confirm.title') : t('team.content.leave.confirm.title'),
                children: (
                  <Text size="sm">
                    {isCaptain
                      ? t('team.content.disband.confirm.message', {
                          team: teamInfo?.name,
                        })
                      : t('team.content.leave.confirm.message', {
                          team: teamInfo?.name,
                        })}
                  </Text>
                ),
                onConfirm: isCaptain ? onConfirmDisbandTeam : onConfirmLeaveTeam,
                confirmProps: { color: 'red' },
                zIndex: 10000,
              })
            }}
          >
            {isCaptain ? t('team.button.disband') : t('team.button.leave')}
          </Button>
          <Button fullWidth disabled={!isCaptain || locked || disabled} loading={disabled} onClick={onSaveChange}>
            {t('team.button.save')}
          </Button>
        </Group>
      </Stack>

      {/* 更新头像浮窗 */}
      <Modal
        opened={dropzoneOpened}
        onClose={() => !disabled && setDropzoneOpened(false)}
        title={avatarModalTitle}
        withCloseButton={!disabled}
        zIndex={1000}
      >
        <VisuallyHidden id="team-avatar-upload-instructions">
          {avatarDropzonePrompt}. {avatarDropzoneLimit}
        </VisuallyHidden>
        <Dropzone
          aria-label={avatarModalTitle}
          aria-describedby="team-avatar-upload-instructions"
          onDrop={(files) => setAvatarFile(files[0])}
          onReject={() => {
            showNotification({
              color: 'red',
              title: t('common.error.file_invalid.title'),
              message: t('common.error.file_invalid.message'),
              icon: <Icon path={mdiClose} size={1} />,
            })
          }}
          m="0 auto 20px auto"
          miw={220}
          mih={220}
          disabled={disabled}
          maxSize={3 * 1024 * 1024}
          accept={IMAGE_MIME_TYPES}
        >
          <Group justify="center" gap="xl" mih={240} className={misc.n}>
            {avatarFile ? (
              <Image
                fit="contain"
                src={URL.createObjectURL(avatarFile)}
                alt={t('team.content.avatar_preview_alt', {
                  defaultValue: 'Preview of the selected avatar for {{team}}',
                  team: teamName,
                })}
              />
            ) : (
              <Box>
                <Text size="xl" inline>
                  {avatarDropzonePrompt}
                </Text>
                <Text size="sm" c="dimmed" inline mt={7}>
                  {avatarDropzoneLimit}
                </Text>
              </Box>
            )}
          </Group>
        </Dropzone>
        <Button fullWidth variant="outline" disabled={disabled} onClick={onChangeAvatar}>
          {t('common.avatar.save')}
        </Button>
      </Modal>
    </Modal>
  )
}
