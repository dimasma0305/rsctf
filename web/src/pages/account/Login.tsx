import { Alert, Anchor, Button, Grid, PasswordInput, TextInput } from '@mantine/core'
import { useDisclosure, useInputState } from '@mantine/hooks'
import { hideNotification, showNotification, updateNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { Captcha, useCaptchaRef } from '@Components/Captcha'
import { OAuthButtons } from '@Components/OAuthButtons'
import { TermsOfService } from '@Components/TermsOfService'
import { accountPath, accountReturnPath } from '@Utils/AccountNavigation'
import { completeSignIn } from '@Utils/CompleteSignIn'
import { encryptApiData } from '@Utils/Crypto'
import { collectEncryptedFingerprintIdentity } from '@Utils/FingerprintIdentity'
import { oauthFailure } from '@Utils/OAuthFailure'
import { tryGetClientError } from '@Utils/Shared'
import { useConfig } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUser } from '@Hooks/useUser'
import api from '@Api'
import misc from '@Styles/Misc.module.css'
import classes from './Login.module.css'

const Login: FC = () => {
  const params = useSearchParams()[0]
  const navigate = useNavigate()
  const from = accountReturnPath(params.get('from'))
  const oauthError = oauthFailure(params.get('error'))

  const [pwd, setPwd] = useInputState('')
  const [uname, setUname] = useInputState('')
  const [unameError, setUnameError] = useState<string | null>(null)
  const [pwdError, setPwdError] = useState<string | null>(null)
  const [disabled, setDisabled] = useState(false)
  const [accepted, setAccepted] = useState(false)
  const [tosOpened, { open: openTos, close: closeTos }] = useDisclosure(false)
  const loginOperationRef = useRef<{ controller: AbortController; running: boolean } | null>(null)

  const { captchaRef, getToken, cleanUp } = useCaptchaRef()
  const { mutate } = useUser()
  const { config } = useConfig()

  const { t } = useTranslation()

  usePageTitle(t('account.title.login'))

  useEffect(
    () => () => {
      if (loginOperationRef.current) {
        loginOperationRef.current.controller.abort()
        hideNotification('login-status')
      }
    },
    []
  )

  const executeLogin = async (consentGranted = false) => {
    const requestUrl = window.location.href
    let operation = loginOperationRef.current
    if (operation?.running) return
    if (operation && !consentGranted) return
    if (!operation) {
      operation = { controller: new AbortController(), running: false }
      loginOperationRef.current = operation
    }

    const unameInvalid = uname.length === 0
    const pwdInvalid = pwd.length < 6
    if (unameInvalid || pwdInvalid) {
      setUnameError(
        unameInvalid ? t('account.validation.username_required', 'Please enter your username or email') : null
      )
      setPwdError(
        pwdInvalid ? t('account.validation.password_min_length', 'Password must be at least 6 characters') : null
      )
      showNotification({
        color: 'red',
        title: t('account.notification.login.check_input', 'Please check your input'),
        message: t('common.error.check_input'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      loginOperationRef.current = null
      return
    }

    setUnameError(null)
    setPwdError(null)

    if (config.enableBrowserFingerprint && !accepted && !consentGranted) {
      openTos()
      return
    }
    operation.running = true
    setDisabled(true)

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
        id: 'login-status',
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
      await api.account.accountLogIn(
        {
          userName: uname,
          password,
          challenge: token,
          fingerprint: fingerprintPayload?.fingerprint,
          fingerprintProof: fingerprintPayload?.fingerprintProof,
        },
        { signal: operation.controller.signal }
      )
      await completeSignIn({
        signal: operation.controller.signal,
        isCurrent: () => window.location.href === requestUrl,
        loadProfile: async (signal) => (await api.account.accountProfile({ signal })).data,
        publishProfile: (profile) => {
          loginOperationRef.current = null
          updateNotification({
            id: 'login-status',
            color: 'teal',
            title: t('account.notification.login.success.title'),
            message: t('account.notification.login.success.message'),
            icon: <Icon path={mdiCheck} size={1} />,
            autoClose: true,
            loading: false,
          })
          cleanUp(true)
          return mutate(profile, { revalidate: false })
        },
        navigate: () => navigate(from, { replace: true }),
      })
    } catch (err: any) {
      if (operation.controller.signal.aborted || window.location.href !== requestUrl) return
      const { title, message } = tryGetClientError(err, t)
      updateNotification({
        id: 'login-status',
        color: 'red',
        title,
        message,
        icon: <Icon path={mdiClose} size={1} />,
        autoClose: true,
        loading: false,
      })
      cleanUp(false)
    } finally {
      if (window.location.href !== requestUrl && loginOperationRef.current === operation) {
        operation.controller.abort()
        hideNotification('login-status')
      }
      if (loginOperationRef.current === operation) loginOperationRef.current = null
      if (!operation.controller.signal.aborted && window.location.href === requestUrl) setDisabled(false)
    }
  }

  const onLogin = async (event: React.SyntheticEvent) => {
    event.preventDefault()
    await executeLogin()
  }

  return (
    <AccountView
      title={t('account.title.login')}
      description={t('account.content.login.description', 'Welcome back. Enter your account details to continue.')}
      onSubmit={onLogin}
    >
      {oauthError && (
        <Alert
          role="alert"
          color={oauthError.pending ? 'orange' : 'red'}
          title={t(oauthError.pending ? 'account.oauth.error.info_title' : 'account.oauth.error.title')}
        >
          {t(oauthError.messageKey)}
        </Alert>
      )}
      <TextInput
        data-guide="account-access"
        required
        label={t('account.label.username_or_email')}
        placeholder="ctfer"
        type="text"
        w="100%"
        value={uname}
        disabled={disabled}
        error={unameError}
        onChange={(event) => {
          setUname(event.currentTarget.value)
          setUnameError(null)
        }}
      />
      <PasswordInput
        required
        label={t('account.label.password')}
        id="your-password"
        placeholder="P4ssW@rd"
        rightSectionWidth="2.75rem"
        classNames={{
          input: classes.passwordField,
          innerInput: classes.passwordInnerInput,
          visibilityToggle: classes.passwordVisibilityToggle,
        }}
        visibilityToggleButtonProps={{
          'aria-label': t('account.button.toggle_password_visibility', 'Toggle password visibility'),
        }}
        w="100%"
        value={pwd}
        disabled={disabled}
        error={pwdError}
        onChange={(event) => {
          setPwd(event.currentTarget.value)
          setPwdError(null)
        }}
      />
      <Captcha action="login" ref={captchaRef} />
      <TermsOfService
        confirmMode
        opened={tosOpened}
        onClose={() => {
          loginOperationRef.current?.controller.abort()
          loginOperationRef.current = null
          closeTos()
        }}
        onAccept={() => {
          setAccepted(true)
          closeTos()
          void executeLogin(true)
        }}
      />
      <Anchor fz="xs" className={misc.alignSelfEnd} component={Link} to={accountPath('/account/recovery', from)}>
        {t('account.anchor.recovery')}
      </Anchor>
      <Grid grow w="100%">
        {config.allowRegister !== false && (
          <Grid.Col span={2}>
            <Button fullWidth variant="outline" component={Link} to={accountPath('/account/register', from)}>
              {t('account.button.register')}
            </Button>
          </Grid.Col>
        )}
        <Grid.Col span={2}>
          <Button type="submit" fullWidth disabled={disabled}>
            {t('account.button.login')}
          </Button>
        </Grid.Col>
      </Grid>
      <OAuthButtons />
    </AccountView>
  )
}

export default Login
