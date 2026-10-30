import { Alert, Anchor, Button, Loader, PasswordInput, Text, TextInput } from '@mantine/core'
import { useDisclosure, useInputState } from '@mantine/hooks'
import { hideNotification, showNotification, updateNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { InvitationNotice } from '@Components/InvitationNotice'
import { Captcha, useCaptchaRef } from '@Components/Captcha'
import { OAuthButtons } from '@Components/OAuthButtons'
import { StrengthPasswordInput } from '@Components/StrengthPasswordInput'
import { TermsOfService } from '@Components/TermsOfService'
import { accountPath, accountReturnPath } from '@Utils/AccountNavigation'
import { completeSignIn } from '@Utils/CompleteSignIn'
import { encryptApiData } from '@Utils/Crypto'
import { collectEncryptedFingerprintIdentity } from '@Utils/FingerprintIdentity'
import { beginMailOperation, finishMailOperation, type MailOperationOwner } from '@Utils/MailOperation'
import { tryGetClientError } from '@Utils/Shared'
import { useConfig } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUser } from '@Hooks/useUser'
import api, { RegisterStatus } from '@Api'
import misc from '@Styles/Misc.module.css'

const Register: FC = () => {
  const [pwd, setPwd] = useInputState('')
  const [retypedPwd, setRetypedPwd] = useInputState('')
  const [uname, setUname] = useInputState('')
  const [email, setEmail] = useInputState('')
  const [bootstrapToken, setBootstrapToken] = useInputState('')
  const [disabled, setDisabled] = useState(false)
  const [accepted, setAccepted] = useState(false)
  const [tosOpened, { open: openTos, close: closeTos }] = useDisclosure(false)
  const registerOperationRef = useRef<MailOperationOwner | null>(null)
  const { config, loading: configLoading, error: configError, mutate: reloadConfig } = useConfig()
  const { mutate: mutateProfile } = useUser()

  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const from = accountReturnPath(searchParams.get('from'))
  const bootstrapMode = searchParams.get('bootstrap') === '1'
  // Open registration already supports authenticated pending-account retries.
  // Expose that existing flow separately when new password signup is closed.
  const resendMode =
    !bootstrapMode &&
    (config.allowRegister === false || config.allowPasswordRegistration === false) &&
    searchParams.get('resend') === '1'
  const { captchaRef, getToken, cleanUp } = useCaptchaRef()

  const { t } = useTranslation()

  const RegisterStatusMap = new Map([
    [
      RegisterStatus.LoggedIn,
      {
        message: t('account.notification.register.logged_in'),
      },
    ],
    [
      RegisterStatus.AdminConfirmationRequired,
      {
        title: t('account.notification.register.request_sent.title'),
        message: t('account.notification.register.request_sent.message'),
      },
    ],
    [
      RegisterStatus.EmailConfirmationRequired,
      {
        title: t('common.email.sent.title'),
        message: t('common.email.sent.message'),
      },
    ],
    [undefined, undefined],
  ])

  usePageTitle(t(resendMode ? 'account.registration.resend_title' : 'account.title.register'))

  useEffect(
    () => () => {
      if (registerOperationRef.current) {
        registerOperationRef.current.controller.abort()
        hideNotification('register-status')
      }
    },
    []
  )

  const executeRegister = async (consentGranted = false) => {
    const requestUrl = window.location.href
    const signature = JSON.stringify([
      uname.trim(),
      email.trim().toLowerCase(),
      pwd,
      retypedPwd,
      bootstrapMode ? bootstrapToken : '',
    ])
    const acquired = beginMailOperation(registerOperationRef.current, signature)
    if (!acquired.started) return
    const operation = acquired.owner
    registerOperationRef.current = operation

    if (!resendMode && pwd !== retypedPwd) {
      showNotification({
        color: 'red',
        title: t('common.error.check_input'),
        message: t('account.password.not_match'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      registerOperationRef.current = finishMailOperation(operation, true)
      return
    }

    if (config.enableBrowserFingerprint && !accepted && !consentGranted) {
      operation.running = false
      openTos()
      return
    }
    setDisabled(true)
    let completed = false

    try {
      const { valid, token } = await getToken()
      if (operation.controller.signal.aborted || window.location.href !== requestUrl) return
      if (!valid) {
        showNotification({
          color: 'orange',
          title: t('account.notification.captcha.not_valid'),
          message: t('common.error.try_later'),
        })
        return
      }

      showNotification({
        color: 'orange',
        id: 'register-status',
        title: t('account.notification.captcha.request_sent.title'),
        message: t('account.notification.captcha.request_sent.message'),
        loading: true,
        autoClose: false,
      })

      const fingerprintPayload = config.enableBrowserFingerprint
        ? await collectEncryptedFingerprintIdentity(t, config.apiPublicKey, operation.controller.signal)
        : undefined

      const password = await encryptApiData(t, pwd, config.apiPublicKey)
      if (operation.controller.signal.aborted || window.location.href !== requestUrl) return
      const res = await api.account.accountRegister(
        {
          userName: uname,
          password,
          email: email,
          challenge: token,
          fingerprint: fingerprintPayload?.fingerprint,
          fingerprintProof: fingerprintPayload?.fingerprintProof,
          bootstrapToken: bootstrapMode ? bootstrapToken : undefined,
          operationId: operation.operationId,
        },
        { signal: operation.controller.signal }
      )
      if (operation.controller.signal.aborted || window.location.href !== requestUrl) return
      completed = true
      const data = RegisterStatusMap.get(res.data.data)
      if (data) {
        const publishSuccess = () =>
          updateNotification({
            id: 'register-status',
            color: 'teal',
            title: data.title,
            message: data.message,
            icon: <Icon path={mdiCheck} size={1} />,
            loading: false,
            autoClose: true,
          })
        if (res.data.data === RegisterStatus.LoggedIn) {
          await completeSignIn({
            signal: operation.controller.signal,
            isCurrent: () => window.location.href === requestUrl,
            loadProfile: async (signal) => (await api.account.accountProfile({ signal })).data,
            publishProfile: (profile) => {
              registerOperationRef.current = null
              publishSuccess()
              cleanUp(true)
              return mutateProfile(profile, { revalidate: false })
            },
            navigate: () => navigate(from, { replace: true }),
          })
        } else {
          registerOperationRef.current = null
          publishSuccess()
          cleanUp(true)
          if (res.data.data === RegisterStatus.EmailConfirmationRequired)
            navigate(accountPath('/account/pending', from), { state: { email } })
          else navigate(accountPath('/account/login', from))
        }
      }
    } catch (err: any) {
      if (operation.controller.signal.aborted || window.location.href !== requestUrl) return
      const { title, message } = tryGetClientError(err, t)

      updateNotification({
        id: 'register-status',
        color: 'red',
        title,
        message,
        icon: <Icon path={mdiClose} size={1} />,
        loading: false,
        autoClose: true,
      })
      cleanUp(false)
    } finally {
      if (window.location.href !== requestUrl && registerOperationRef.current === operation) {
        operation.controller.abort()
        hideNotification('register-status')
      }
      if (registerOperationRef.current === operation)
        registerOperationRef.current = finishMailOperation(operation, completed)
      if (!operation.controller.signal.aborted && window.location.href === requestUrl) setDisabled(false)
    }
  }

  const onRegister = async (event: React.SyntheticEvent) => {
    event.preventDefault()
    await executeRegister()
  }

  if (configLoading || configError) {
    return (
      <AccountView
        title={t('account.title.register')}
        description={t(
          'account.content.register.description',
          'Create an account and get ready for the next challenge.'
        )}
      >
        {configLoading ? (
          <Loader role="status" aria-label={t('common.content.loading', 'Loading')} />
        ) : (
          <Alert role="alert" color="red" title={t('account.registration.policy_error_title')}>
            <Text size="sm">{t('account.registration.policy_error')}</Text>
            <Button type="button" mt="sm" onClick={() => void reloadConfig()}>
              {t('common.button.retry', 'Retry')}
            </Button>
          </Alert>
        )}
        <Anchor component={Link} to={accountPath('/account/login', from)}>
          {t('account.anchor.login')}
        </Anchor>
      </AccountView>
    )
  }

  if (!bootstrapMode && !resendMode && config.allowRegister === false) {
    return (
      <AccountView
        title={t('account.registration.closed_title')}
        description={t('account.registration.closed_description')}
      >
        <Anchor component={Link} to={accountPath('/account/register?resend=1', from)}>
          {t('account.registration.resend_link')}
        </Anchor>
        <Anchor component={Link} to={accountPath('/account/login', from)}>
          {t('account.anchor.login')}
        </Anchor>
      </AccountView>
    )
  }

  if (!bootstrapMode && !resendMode && config.allowPasswordRegistration === false) {
    const providerAvailable = Boolean(config.enableGoogleAuth || config.enableDiscordAuth)
    return (
      <AccountView title={t('account.title.register')} description={t('account.oauth.registration_only_description')}>
        <Alert color={providerAvailable ? 'blue' : 'red'}>
          <Text size="sm">
            {t(providerAvailable ? 'account.oauth.registration_only_notice' : 'account.oauth.registration_unavailable')}
          </Text>
        </Alert>
        {providerAvailable && <OAuthButtons />}
        <Anchor fz="xs" className={misc.alignSelfEnd} component={Link} to={accountPath('/account/login', from)}>
          {t('account.anchor.login')}
        </Anchor>
      </AccountView>
    )
  }

  return (
    <AccountView
      title={t(resendMode ? 'account.registration.resend_title' : 'account.title.register')}
      description={
        resendMode
          ? t('account.registration.resend_description')
          : t('account.content.register.description', 'Create an account and get ready for the next challenge.')
      }
      onSubmit={onRegister}
    >
      <InvitationNotice from={from} />
      {bootstrapMode && (
        <PasswordInput
          required
          label={t('account.label.bootstrap_token', 'Setup token')}
          description={t(
            'account.content.register.bootstrap_token',
            'Copy the setup token shown by the installer or in your first-start Docker logs. For Kubernetes, follow the Helm notes.'
          )}
          value={bootstrapToken}
          disabled={disabled}
          onChange={(event) => setBootstrapToken(event.currentTarget.value)}
          w="100%"
          autoComplete="off"
        />
      )}
      <TextInput
        data-guide="account-access"
        required
        label={t('account.label.email')}
        type="email"
        placeholder="ctf@example.com"
        w="100%"
        value={email}
        disabled={disabled}
        onChange={(event) => setEmail(event.currentTarget.value)}
      />
      <TextInput
        required
        label={t('account.label.username')}
        type="text"
        placeholder="ctfer"
        w="100%"
        value={uname}
        disabled={disabled}
        onChange={(event) => setUname(event.currentTarget.value)}
      />
      {resendMode ? (
        <PasswordInput
          required
          label={t('account.label.password')}
          autoComplete="current-password"
          value={pwd}
          onChange={(event) => setPwd(event.currentTarget.value)}
          disabled={disabled}
          w="100%"
        />
      ) : (
        <>
          <StrengthPasswordInput
            value={pwd}
            onChange={(event) => setPwd(event.currentTarget.value)}
            disabled={disabled}
          />
          <PasswordInput
            required
            label={t('account.label.password_retype')}
            value={retypedPwd}
            onChange={(event) => setRetypedPwd(event.currentTarget.value)}
            disabled={disabled}
            w="100%"
            error={retypedPwd.length > 0 && pwd !== retypedPwd && t('account.password.not_match')}
          />
        </>
      )}
      <Captcha action="register" ref={captchaRef} />
      <TermsOfService
        confirmMode
        opened={tosOpened}
        onClose={() => {
          registerOperationRef.current?.controller.abort()
          registerOperationRef.current = null
          closeTos()
        }}
        onAccept={() => {
          setAccepted(true)
          closeTos()
          void executeRegister(true)
        }}
      />
      <Anchor fz="xs" className={misc.alignSelfEnd} component={Link} to={accountPath('/account/login', from)}>
        {t('account.anchor.login')}
      </Anchor>
      <Button type="submit" fullWidth disabled={disabled}>
        {t(resendMode ? 'account.registration.resend_title' : 'account.button.register')}
      </Button>
      {!resendMode && <OAuthButtons />}
    </AccountView>
  )
}

export default Register
