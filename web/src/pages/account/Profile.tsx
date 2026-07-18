import {
  Avatar,
  Box,
  Button,
  Grid,
  Group,
  Image,
  Modal,
  Paper,
  PasswordInput,
  SimpleGrid,
  Stack,
  Tabs,
  Text,
  Textarea,
  TextInput,
  Title,
  Tooltip,
  VisuallyHidden,
} from '@mantine/core'
import { Dropzone } from '@mantine/dropzone'
import { notifications, showNotification, updateNotification } from '@mantine/notifications'
import { mdiAccountOutline, mdiChartBox, mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useMemo, useState } from 'react'
import { Trans, useTranslation } from 'react-i18next'
import { useSearchParams } from 'react-router'
import { PasswordChangeModal } from '@Components/PasswordChangeModal'
import { WithNavBar } from '@Components/WithNavbar'
import { StatsPanel } from '@Components/account/StatsPanel'
import { showErrorMsg, tryGetErrorMsg } from '@Utils/Shared'
import { IMAGE_MIME_TYPES } from '@Utils/Shared'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUser } from '@Hooks/useUser'
import api, { ProfileUpdateModel } from '@Api'
import misc from '@Styles/Misc.module.css'

const Profile: FC = () => {
  const [dropzoneOpened, setDropzoneOpened] = useState(false)
  const { user, mutate } = useUser()

  const [searchParams, setSearchParams] = useSearchParams()
  const activeTab = searchParams.get('tab') === 'stats' ? 'stats' : 'profile'
  const setActiveTab = (tab: string | null) =>
    setSearchParams(tab && tab !== 'profile' ? { tab } : {}, { replace: true })

  const [profile, setProfile] = useState<ProfileUpdateModel>({
    userName: user?.userName,
    bio: user?.bio,
    stdNumber: user?.stdNumber,
    phone: user?.phone,
    realName: user?.realName,
  })
  const [avatarFile, setAvatarFile] = useState<File | null>(null)

  const avatarPreview = useMemo(() => (avatarFile ? URL.createObjectURL(avatarFile) : null), [avatarFile])

  useEffect(() => {
    if (!avatarPreview) return
    return () => URL.revokeObjectURL(avatarPreview)
  }, [avatarPreview])

  const [disabled, setDisabled] = useState(false)

  const [mailEditOpened, setMailEditOpened] = useState(false)
  const [pwdChangeOpened, setPwdChangeOpened] = useState(false)

  const [email, setEmail] = useState('')
  const [emailPassword, setEmailPassword] = useState('')

  const { t } = useTranslation()
  const avatarModalTitle = t('account.button.change_avatar', { defaultValue: 'Change avatar' })
  const avatarDropzonePrompt = t('common.content.drop_zone.content', {
    defaultValue: 'Drag and drop an image or click here to select {{type}}',
    type: t('common.content.drop_zone.type.avatar', { defaultValue: 'avatar' }),
  })
  const avatarDropzoneLimit = t('common.content.drop_zone.limit', {
    defaultValue: 'Please select an image less than 3MB',
  })

  usePageTitle(t('account.title.profile'))

  useEffect(() => {
    setProfile({
      userName: user?.userName,
      bio: user?.bio,
      stdNumber: user?.stdNumber,
      phone: user?.phone,
      realName: user?.realName,
    })
  }, [user])

  const onChangeAvatar = async () => {
    if (!avatarFile) return

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
      await api.account.accountAvatar({ file: avatarFile })
      updateNotification({
        id: 'upload-avatar',
        color: 'teal',
        message: t('common.avatar.uploaded'),
        icon: <Icon path={mdiCheck} size={1} />,
        autoClose: true,
        loading: false,
      })
      setDisabled(false)
      mutate()
      setAvatarFile(null)
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
      setDisabled(false)
      setDropzoneOpened(false)
    }
  }

  const onChangeProfile = async () => {
    try {
      setDisabled(true)
      await api.account.accountUpdate(profile)
      showNotification({
        color: 'teal',
        message: t('account.notification.profile.profile_updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
      mutate({ ...user })
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      // Without this the whole account form (every input + button) stays greyed out
      // after a successful save — the user would have to reload to edit again.
      setDisabled(false)
    }
  }

  const onChangeEmail = async () => {
    if (!email) return

    try {
      setDisabled(true)
      const res = await api.account.accountChangeEmail({ newMail: email, password: emailPassword })
      if (res.data.data) {
        showNotification({
          color: 'teal',
          title: t('common.email.sent.title'),
          message: t('common.email.sent.message'),
          icon: <Icon path={mdiCheck} size={1} />,
        })
      } else {
        mutate({ ...user, email: email })
        showNotification({
          color: 'teal',
          title: t('account.notification.profile.email_updated.title', 'Email updated'),
          message: t('account.notification.profile.email_updated.message', 'Your email address has been changed.'),
          icon: <Icon path={mdiCheck} size={1} />,
        })
      }
      setEmail('')
      setEmailPassword('')
      setMailEditOpened(false)
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setDisabled(false)
    }
  }

  const profilePanel = (
    <Paper withBorder radius="md" p="lg" maw={640} mx="auto" w="100%">
      <Stack gap="md">
        <TextInput
          label={t('account.label.username')}
          type="text"
          w="100%"
          value={profile.userName ?? 'ctfer'}
          disabled={disabled}
          onChange={(event) => setProfile({ ...profile, userName: event.target.value })}
        />
        <SimpleGrid cols={{ base: 1, xs: 2 }}>
          <TextInput
            label={t('account.label.email')}
            type="email"
            w="100%"
            value={user?.email ?? 'player@example.com'}
            disabled
            readOnly
          />
          <TextInput
            label={t('account.label.phone')}
            type="tel"
            w="100%"
            value={profile.phone ?? ''}
            disabled={disabled}
            onChange={(event) => setProfile({ ...profile, phone: event.target.value })}
          />
          <TextInput
            label={t('account.label.student_id')}
            type="text"
            w="100%"
            value={profile.stdNumber ?? ''}
            disabled={disabled}
            onChange={(event) => setProfile({ ...profile, stdNumber: event.target.value })}
          />
          <TextInput
            label={t('account.label.real_name')}
            type="text"
            w="100%"
            value={profile.realName ?? ''}
            disabled={disabled}
            onChange={(event) => setProfile({ ...profile, realName: event.target.value })}
          />
        </SimpleGrid>
        <Textarea
          label={t('account.label.bio')}
          value={profile.bio ?? t('account.placeholder.bio')}
          w="100%"
          disabled={disabled}
          autosize
          minRows={2}
          maxRows={4}
          onChange={(event) => setProfile({ ...profile, bio: event.target.value })}
        />
        <Grid grow>
          <Grid.Col span={{ base: 12, xs: 4 }}>
            <Button fullWidth variant="outline" disabled={disabled} onClick={() => setMailEditOpened(true)}>
              {t('account.button.update_email')}
            </Button>
          </Grid.Col>
          <Grid.Col span={{ base: 12, xs: 4 }}>
            <Button fullWidth variant="outline" disabled={disabled} onClick={() => setPwdChangeOpened(true)}>
              {t('account.button.change_password')}
            </Button>
          </Grid.Col>
          <Grid.Col span={{ base: 12, xs: 4 }}>
            <Button fullWidth disabled={disabled} onClick={onChangeProfile}>
              {t('account.button.save_profile')}
            </Button>
          </Grid.Col>
        </Grid>
      </Stack>
    </Paper>
  )

  return (
    <WithNavBar>
      <Stack p="md" maw={880} mx="auto" gap="lg" w="100%">
        {/* Shared header */}
        <Group wrap="nowrap">
          <Tooltip label={avatarModalTitle} withArrow>
            <Avatar
              src={user?.avatar}
              size={56}
              radius="md"
              color="brand"
              role="button"
              tabIndex={0}
              style={{ cursor: 'pointer' }}
              onClick={() => setDropzoneOpened(true)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault()
                  setDropzoneOpened(true)
                }
              }}
            >
              {user?.userName?.[0]?.toUpperCase()}
            </Avatar>
          </Tooltip>
          <div>
            <Title order={1} size="h3" lineClamp={1}>
              {user?.userName ?? t('account.title.profile')}
            </Title>
            <Text size="sm" c="dimmed">
              {user?.email}
            </Text>
          </div>
        </Group>

        <Tabs value={activeTab} onChange={setActiveTab} keepMounted={false}>
          <Tabs.List mb="md">
            <Tabs.Tab value="profile" leftSection={<Icon path={mdiAccountOutline} size={0.8} />}>
              {t('account.title.profile')}
            </Tabs.Tab>
            <Tabs.Tab value="stats" leftSection={<Icon path={mdiChartBox} size={0.8} />}>
              {t('account.title.stats', 'My Stats')}
            </Tabs.Tab>
          </Tabs.List>

          <Tabs.Panel value="profile">{profilePanel}</Tabs.Panel>
          <Tabs.Panel value="stats">
            <StatsPanel />
          </Tabs.Panel>
        </Tabs>
      </Stack>

      <PasswordChangeModal
        opened={pwdChangeOpened}
        onClose={() => setPwdChangeOpened(false)}
        title={t('account.button.change_password')}
      />

      <Modal opened={mailEditOpened} onClose={() => setMailEditOpened(false)} title={t('account.button.update_email')}>
        <Stack>
          <Text>
            <Trans i18nKey="account.content.profile.update_email_note"></Trans>
          </Text>
          <TextInput
            required
            label={t('account.label.email_new')}
            type="email"
            w="100%"
            placeholder={user?.email ?? 'player@example.com'}
            value={email}
            onChange={(event) => setEmail(event.target.value)}
          />
          <PasswordInput
            required
            label={t('account.label.password_current', 'Current password')}
            autoComplete="current-password"
            value={emailPassword}
            onChange={(event) => setEmailPassword(event.currentTarget.value)}
          />
          <Group justify="right">
            <Button
              variant="default"
              onClick={() => {
                setEmail(user?.email ?? '')
                setEmailPassword('')
                setMailEditOpened(false)
              }}
            >
              {t('common.modal.cancel')}
            </Button>
            <Button color="orange" onClick={onChangeEmail}>
              {t('common.modal.confirm')}
            </Button>
          </Group>
        </Stack>
      </Modal>

      <Modal opened={dropzoneOpened} onClose={() => setDropzoneOpened(false)} title={avatarModalTitle} withCloseButton>
        <VisuallyHidden id="profile-avatar-upload-instructions">
          {avatarDropzonePrompt}. {avatarDropzoneLimit}
        </VisuallyHidden>
        <Dropzone
          aria-label={avatarModalTitle}
          aria-describedby="profile-avatar-upload-instructions"
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
          maxSize={3 * 1024 * 1024}
          accept={IMAGE_MIME_TYPES}
        >
          <Group justify="center" gap="xl" mih={240} className={misc.noPointerEvents}>
            {avatarPreview ? (
              <Image
                fit="contain"
                src={avatarPreview}
                alt={t('account.content.avatar_preview_alt', {
                  defaultValue: 'Preview of your selected avatar',
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
    </WithNavBar>
  )
}

export default Profile
