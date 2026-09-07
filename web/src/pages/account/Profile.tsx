import {
  Alert,
  Avatar,
  Box,
  Button,
  Group,
  Image,
  Modal,
  PasswordInput,
  SimpleGrid,
  Skeleton,
  Stack,
  Tabs,
  Text,
  Textarea,
  TextInput,
  Title,
  VisuallyHidden,
} from '@mantine/core'
import { Dropzone } from '@mantine/dropzone'
import { notifications, showNotification, updateNotification } from '@mantine/notifications'
import { mdiAccountOutline, mdiCameraOutline, mdiChartBox, mdiCheck, mdiClose, mdiLockOutline } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useMemo, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useSearchParams } from 'react-router'
import { PageHeader } from '@Components/PageHeader'
import { PasswordChangeModal } from '@Components/PasswordChangeModal'
import { WithNavBar } from '@Components/WithNavbar'
import { StatsPanel } from '@Components/account/StatsPanel'
import { BlobUploadOperation, retainBlobUploadOperation } from '@Utils/BlobUploadOperations'
import { beginMailOperation, finishMailOperation, type MailOperationOwner } from '@Utils/MailOperation'
import { profileFields, refreshedProfileDraft, sameProfile } from '@Utils/ProfileDraft'
import { profileErrorDisposition } from '@Utils/ProfileRetry'
import { showErrorMsg, tryGetErrorMsg } from '@Utils/Shared'
import { IMAGE_MIME_TYPES } from '@Utils/Shared'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUser } from '@Hooks/useUser'
import api from '@Api'
import misc from '@Styles/Misc.module.css'
import classes from '@Styles/Profile.module.css'

const Profile: FC = () => {
  const [dropzoneOpened, setDropzoneOpened] = useState(false)
  const { user, error, mutate } = useUser()

  const [searchParams, setSearchParams] = useSearchParams()
  const activeTab = searchParams.get('tab') === 'stats' ? 'stats' : 'profile'
  const setActiveTab = (tab: string | null) => {
    const next = new URLSearchParams(searchParams)
    if (tab === 'stats') next.set('tab', tab)
    else next.delete('tab')
    setSearchParams(next, { replace: true })
  }

  const [profile, setProfile] = useState(() => profileFields(user))
  const previousUser = useRef(user)
  const profileInFlight = useRef(false)
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState('')
  const [saved, setSaved] = useState(false)
  const dirty = !sameProfile(profile, profileFields(user))
  const [avatarFile, setAvatarFile] = useState<File | null>(null)
  const avatarOperation = useRef<BlobUploadOperation | null>(null)

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
  const mailOperationRef = useRef<MailOperationOwner | null>(null)

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
    const previous = previousUser.current
    setProfile((draft) => refreshedProfileDraft(draft, previous, user))
    previousUser.current = user
  }, [user])

  const editField = (field: keyof typeof profile, value: string) => {
    setProfile((draft) => ({ ...draft, [field]: value }))
    setSaved(false)
    setSaveError('')
  }

  const discardProfile = () => {
    setProfile(profileFields(user))
    setSaveError('')
    setSaved(false)
  }

  const closeEmail = () => {
    if (disabled) return
    mailOperationRef.current?.controller.abort()
    mailOperationRef.current = null
    setMailEditOpened(false)
    setEmail('')
    setEmailPassword('')
  }

  const onChangeAvatar = async () => {
    if (!avatarFile || disabled) return

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
      avatarOperation.current = retainBlobUploadOperation(avatarOperation.current, avatarFile)
      await api.account.accountAvatar({ file: avatarFile }, avatarOperation.current.id)
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
      avatarOperation.current = null
      setDropzoneOpened(false)
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
    }
  }

  const onChangeProfile = async () => {
    if (!user || !dirty || profileInFlight.current || disabled) return
    const submitted = { ...profile, userName: profile.userName.trim() }
    if (submitted.userName.length < 3) {
      setSaveError(t('account.profile_ui.username_short'))
      return
    }
    profileInFlight.current = true
    setSaveError('')
    setSaved(false)
    try {
      setDisabled(true)
      setSaving(true)
      await api.account.accountUpdate(submitted)
      setProfile(submitted)
      await mutate((current) => (current?.userId === user.userId ? { ...current, ...submitted } : current), {
        revalidate: false,
      })
      setSaved(true)
      showNotification({
        color: 'teal',
        message: t('account.notification.profile.profile_updated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      setSaveError(tryGetErrorMsg(e, t))
    } finally {
      profileInFlight.current = false
      setSaving(false)
      setDisabled(false)
    }
  }

  const onChangeEmail = async () => {
    if (!email || !emailPassword || disabled) return
    const signature = JSON.stringify([email.trim().toLowerCase(), emailPassword])
    const acquired = beginMailOperation(mailOperationRef.current, signature)
    if (!acquired.started) return
    const operation = acquired.owner
    mailOperationRef.current = operation
    let completed = false
    try {
      setDisabled(true)
      const res = await api.account.accountChangeEmail(
        { newMail: email, password: emailPassword, operationId: operation.operationId },
        { signal: operation.controller.signal }
      )
      completed = true
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
      if (!operation.controller.signal.aborted) showErrorMsg(e, t)
    } finally {
      if (mailOperationRef.current === operation) mailOperationRef.current = finishMailOperation(operation, completed)
      setDisabled(false)
    }
  }

  const profilePanel = (
    <form
      className={classes.card}
      data-profile-form
      aria-labelledby="profile-details-title"
      aria-busy={saving}
      onSubmit={(event) => {
        event.preventDefault()
        void onChangeProfile()
      }}
    >
      <section className={classes.formSection} aria-labelledby="profile-details-title">
        <Title order={2} id="profile-details-title" className={classes.sectionTitle}>
          {t('account.profile_ui.details_title')}
        </Title>
        <Text className={classes.description}>{t('account.profile_ui.details_description')}</Text>
        <Stack gap="md" mt="lg">
          <TextInput
            data-guide="account-access"
            label={t('account.label.username')}
            type="text"
            name="userName"
            autoComplete="username"
            required
            minLength={3}
            value={profile.userName}
            disabled={disabled}
            maxLength={128}
            onChange={(event) => editField('userName', event.target.value)}
          />
          <Textarea
            label={t('account.label.bio')}
            name="bio"
            value={profile.bio}
            placeholder={t('account.profile_ui.bio_placeholder')}
            disabled={disabled}
            maxLength={4096}
            autosize
            minRows={3}
            maxRows={7}
            onChange={(event) => editField('bio', event.target.value)}
          />
        </Stack>
      </section>
      <section className={classes.formSection} aria-labelledby="profile-contact-title">
        <Group justify="space-between" gap="xs" mb={6}>
          <Title order={2} id="profile-contact-title" className={classes.sectionTitle}>
            {t('account.profile_ui.contact_title')}
          </Title>
          <Text className={classes.optional}>{t('account.profile_ui.optional')}</Text>
        </Group>
        <Text className={classes.description} mb="lg">
          {t('account.profile_ui.contact_description')}
        </Text>
        <SimpleGrid cols={{ base: 1, xs: 2 }}>
          <TextInput
            label={t('account.label.real_name')}
            name="realName"
            autoComplete="name"
            value={profile.realName}
            disabled={disabled}
            maxLength={256}
            onChange={(event) => editField('realName', event.target.value)}
          />
          <TextInput
            label={t('account.label.phone')}
            type="tel"
            name="phone"
            autoComplete="tel"
            value={profile.phone}
            disabled={disabled}
            maxLength={64}
            onChange={(event) => editField('phone', event.target.value)}
          />
          <TextInput
            label={t('account.label.student_id')}
            type="text"
            name="stdNumber"
            value={profile.stdNumber}
            disabled={disabled}
            maxLength={128}
            onChange={(event) => editField('stdNumber', event.target.value)}
          />
        </SimpleGrid>
      </section>
      {saveError && (
        <Alert color="red" role="alert" mx="lg" mb="md" title={t('account.profile_ui.save_failed')}>
          {saveError}
        </Alert>
      )}
      <div className={classes.saveBar}>
        <Text role="status" aria-live="polite" className={classes.saveStatus}>
          {saving
            ? t('account.profile_ui.saving')
            : dirty
              ? t('account.profile_ui.unsaved')
              : saved
                ? t('account.notification.profile.profile_updated')
                : t('account.profile_ui.up_to_date')}
        </Text>
        <Group gap="xs" className={classes.saveActions}>
          <Button type="button" variant="default" disabled={disabled || !dirty} onClick={discardProfile}>
            {t('account.profile_ui.discard')}
          </Button>
          <Button type="submit" disabled={disabled || !dirty} loading={saving}>
            {t('account.button.save_profile')}
          </Button>
        </Group>
      </div>
    </form>
  )

  return (
    <WithNavBar>
      <div className={classes.page} data-profile-page>
        <PageHeader
          eyebrow={t('account.profile_ui.eyebrow')}
          title={t('account.title.profile')}
          description={t('account.profile_ui.description')}
        />
        {!user ? (
          error ? (
            <Alert role="alert" title={t('account.profile_ui.load_error')} mt="lg">
              {profileErrorDisposition(error) === 'anonymous' ? (
                <Button component={Link} to="/account/login?from=%2Faccount%2Fprofile" mt="sm">
                  {t('account.button.login')}
                </Button>
              ) : (
                <Button variant="default" mt="sm" onClick={() => void mutate()}>
                  {t('common.button.retry', 'Retry')}
                </Button>
              )}
            </Alert>
          ) : (
            <Stack mt="lg" role="status" aria-label={t('account.profile_ui.loading')} data-profile-loading>
              <Skeleton h={120} animate={false} />
              <Skeleton h={420} animate={false} />
            </Stack>
          )
        ) : (
          <div className={classes.layout}>
            <section className={`${classes.card} ${classes.identity}`} aria-label={t('account.profile_ui.identity')}>
              <div className={classes.identityTop}>
                <Avatar
                  src={user.avatar}
                  size={72}
                  radius="lg"
                  color="brand"
                  className={classes.avatar}
                  aria-hidden="true"
                >
                  {user.userName?.[0]?.toUpperCase()}
                </Avatar>
                <div className={classes.identityCopy}>
                  <Text className={classes.kicker}>{t('account.profile_ui.identity')}</Text>
                  <Text className={classes.userName}>{user.userName}</Text>
                  <Text className={classes.email}>{user.email}</Text>
                </div>
              </div>
              <Button
                variant="default"
                disabled={disabled}
                onClick={() => setDropzoneOpened(true)}
                leftSection={<Icon path={mdiCameraOutline} size={0.8} aria-hidden="true" />}
              >
                {avatarModalTitle}
              </Button>
            </section>

            <Tabs className={classes.editor} value={activeTab} onChange={setActiveTab} keepMounted={false}>
              <Tabs.List mb="md" aria-label={t('account.profile_ui.sections')}>
                <Tabs.Tab value="profile" leftSection={<Icon path={mdiAccountOutline} size={0.8} aria-hidden="true" />}>
                  {t('account.title.profile')}
                </Tabs.Tab>
                <Tabs.Tab value="stats" leftSection={<Icon path={mdiChartBox} size={0.8} aria-hidden="true" />}>
                  {t('account.title.stats', 'My Stats')}
                </Tabs.Tab>
              </Tabs.List>

              <Tabs.Panel value="profile">{profilePanel}</Tabs.Panel>
              <Tabs.Panel value="stats">
                <StatsPanel />
              </Tabs.Panel>
            </Tabs>

            <section className={`${classes.card} ${classes.security}`} aria-labelledby="profile-security-title">
              <Group gap="xs" wrap="nowrap" mb="xs">
                <Icon path={mdiLockOutline} size={0.9} aria-hidden="true" />
                <Title order={2} id="profile-security-title" className={classes.sectionTitle}>
                  {t('account.profile_ui.security_title')}
                </Title>
              </Group>
              <Text className={classes.description}>{t('account.profile_ui.security_description')}</Text>
              <Stack gap="sm" mt="lg">
                <Button variant="default" disabled={disabled} onClick={() => setMailEditOpened(true)}>
                  {t('account.button.update_email')}
                </Button>
                <Button variant="default" disabled={disabled} onClick={() => setPwdChangeOpened(true)}>
                  {t('account.button.change_password')}
                </Button>
              </Stack>
            </section>
          </div>
        )}
      </div>

      <PasswordChangeModal
        opened={pwdChangeOpened}
        onClose={() => setPwdChangeOpened(false)}
        title={t('account.button.change_password')}
        closeButtonProps={{ 'aria-label': t('account.profile_ui.close') }}
      />

      <Modal
        opened={mailEditOpened}
        onClose={closeEmail}
        closeOnClickOutside={!disabled}
        closeOnEscape={!disabled}
        title={t('account.button.update_email')}
        closeButtonProps={{ 'aria-label': t('account.profile_ui.close') }}
        withCloseButton={!disabled}
      >
        <form
          onSubmit={(event) => {
            event.preventDefault()
            void onChangeEmail()
          }}
        >
          <Stack>
            <Text size="sm" className={classes.description}>
              {t('account.profile_ui.email_note')}
            </Text>
            <TextInput
              required
              label={t('account.label.email_new')}
              type="email"
              autoComplete="email"
              w="100%"
              placeholder={user?.email ?? ''}
              value={email}
              disabled={disabled}
              onChange={(event) => setEmail(event.target.value)}
            />
            <PasswordInput
              required
              label={t('account.label.password_current', 'Current password')}
              autoComplete="current-password"
              value={emailPassword}
              disabled={disabled}
              onChange={(event) => setEmailPassword(event.currentTarget.value)}
            />
            <Group justify="right">
              <Button type="button" variant="default" disabled={disabled} onClick={closeEmail}>
                {t('common.modal.cancel')}
              </Button>
              <Button type="submit" disabled={disabled || !email.trim() || !emailPassword} loading={disabled}>
                {t('common.modal.confirm')}
              </Button>
            </Group>
          </Stack>
        </form>
      </Modal>

      <Modal
        opened={dropzoneOpened}
        onClose={() => {
          if (!disabled) setDropzoneOpened(false)
        }}
        title={avatarModalTitle}
        closeButtonProps={{ 'aria-label': t('account.profile_ui.close') }}
        withCloseButton={!disabled}
        closeOnEscape={!disabled}
        closeOnClickOutside={!disabled}
      >
        <VisuallyHidden id="profile-avatar-upload-instructions">
          {avatarDropzonePrompt}. {avatarDropzoneLimit}
        </VisuallyHidden>
        <Dropzone
          aria-label={avatarModalTitle}
          inputProps={{ 'aria-label': avatarModalTitle }}
          aria-describedby="profile-avatar-upload-instructions"
          onDrop={(files) => {
            const file = files[0]
            avatarOperation.current = retainBlobUploadOperation(avatarOperation.current, file)
            setAvatarFile(file)
          }}
          onReject={() => {
            showNotification({
              color: 'red',
              title: t('common.error.file_invalid.title'),
              message: t('common.error.file_invalid.message'),
              icon: <Icon path={mdiClose} size={1} />,
            })
          }}
          m="0 auto 20px auto"
          disabled={disabled}
          multiple={false}
          mih={160}
          maxSize={3 * 1024 * 1024}
          accept={IMAGE_MIME_TYPES}
        >
          <Group justify="center" gap="xl" mih={160} className={misc.noPointerEvents}>
            {avatarPreview ? (
              <Image
                fit="contain"
                h={200}
                src={avatarPreview}
                alt={t('account.content.avatar_preview_alt', {
                  defaultValue: 'Preview of your selected avatar',
                })}
              />
            ) : (
              <Box>
                <Text size="sm">{avatarDropzonePrompt}</Text>
                <Text size="sm" c="dimmed" inline mt={7}>
                  {avatarDropzoneLimit}
                </Text>
              </Box>
            )}
          </Group>
        </Dropzone>
        <Button fullWidth disabled={disabled || !avatarFile} loading={disabled} onClick={onChangeAvatar}>
          {t('common.avatar.save')}
        </Button>
      </Modal>
    </WithNavBar>
  )
}

export default Profile
