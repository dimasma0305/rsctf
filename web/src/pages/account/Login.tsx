import { Anchor, Button, Grid, PasswordInput, TextInput } from '@mantine/core'
import { useDisclosure, useInputState } from '@mantine/hooks'
import { showNotification, updateNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { Captcha, useCaptchaRef } from '@Components/Captcha'
import { OAuthButtons } from '@Components/OAuthButtons'
import { TermsOfService } from '@Components/TermsOfService'
import { encryptApiData } from '@Utils/Crypto'
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

  const [pwd, setPwd] = useInputState('')
  const [uname, setUname] = useInputState('')
  const [unameError, setUnameError] = useState<string | null>(null)
  const [pwdError, setPwdError] = useState<string | null>(null)
  const [disabled, setDisabled] = useState(false)
  const [needRedirect, setNeedRedirect] = useState(false)
  const [accepted, setAccepted] = useState(false)
  const [tosOpened, { open: openTos, close: closeTos }] = useDisclosure(false)

  const { captchaRef, getToken, cleanUp } = useCaptchaRef()
  const { user, mutate } = useUser()
  const { config } = useConfig()

  const { t } = useTranslation()

  usePageTitle(t('account.title.login'))

  useEffect(() => {
    if (needRedirect && user) {
      setNeedRedirect(false)
      setTimeout(() => {
        navigate(params.get('from') ?? '/')
      }, 200)
    }
  }, [user, needRedirect])

  // Surface OAuth callback errors redirected here as ?error=oauth_* (the backend redirects
  // to /account/login on any external sign-in failure or admin-approval-pending outcome).
  useEffect(() => {
    const error = params.get('error')
    if (!error?.startsWith('oauth_')) return

    const messages: Record<string, string> = {
      oauth_await_approval: t(
        'account.oauth.error.await_approval',
        'Your account was created and is awaiting administrator approval.'
      ),
      oauth_register_disabled: t('account.oauth.error.register_disabled', 'Registration is currently disabled.'),
      oauth_email_unverified: t(
        'account.oauth.error.email_unverified',
        'Your provider account email is not verified, so it cannot be used to sign in.'
      ),
      oauth_email_conflict: t(
        'account.oauth.error.email_conflict',
        'An unverified account already uses this email. Verify that account or contact an administrator.'
      ),
      oauth_no_email: t('account.oauth.error.no_email', 'The provider did not share an email address.'),
      oauth_email_domain: t('account.oauth.error.email_domain', 'Your email domain is not allowed on this platform.'),
      oauth_account_disabled: t('account.oauth.error.account_disabled', 'This account has been disabled.'),
      oauth_anti_cheat: t(
        'account.oauth.error.anti_cheat',
        'Sign-in was blocked by an anti-cheat policy (duplicate IP or device).'
      ),
    }
    const isInfo = error === 'oauth_await_approval'
    showNotification({
      color: isInfo ? 'orange' : 'red',
      title: isInfo
        ? t('account.oauth.error.info_title', 'Almost there')
        : t('account.oauth.error.title', 'Sign-in failed'),
      message:
        messages[error] ??
        t('account.oauth.error.generic', 'External sign-in failed. Please try again or use your password.'),
      icon: <Icon path={isInfo ? mdiCheck : mdiClose} size={1} />,
    })
  }, [])

  const executeLogin = async () => {
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
      setDisabled(false)
      return
    }

    setUnameError(null)
    setPwdError(null)

    if (config.enableBrowserFingerprint && !accepted) {
      openTos()
      return
    }

    const { valid, token } = await getToken()

    if (!valid) {
      showNotification({
        color: 'orange',
        title: t('account.notification.captcha.not_valid'),
        message: t('common.error.try_later'),
        loading: true,
      })
      return
    }

    setDisabled(true)

    showNotification({
      color: 'orange',
      id: 'login-status',
      title: t('account.notification.captcha.request_sent.title'),
      message: t('account.notification.captcha.request_sent.message'),
      loading: true,
      autoClose: false,
    })

    try {
      const fingerprintPayload = config.enableBrowserFingerprint
        ? await (async () => {
            // Avoid loading/running fingerprinting code unless the feature is enabled.
            const challengeResponse = await api.account.accountFingerprintChallenge()
            const challenge = challengeResponse.data.data
            if (!challenge?.nonce || !challenge.requiredSignals) {
              throw new Error('Invalid fingerprint challenge')
            }

            const { getFingerprintPayload } = await import('@Utils/BrowserFingerprint')
            const payload = await getFingerprintPayload({
              nonce: challenge.nonce,
              requiredSignals: challenge.requiredSignals,
            })
            return {
              fingerprint: await encryptApiData(t, payload.fingerprint, config.apiPublicKey),
              fingerprintProof: await encryptApiData(t, payload.proof, config.apiPublicKey),
            }
          })()
        : undefined

      await api.account.accountLogIn({
        userName: uname,
        password: await encryptApiData(t, pwd, config.apiPublicKey),
        challenge: token,
        fingerprint: fingerprintPayload?.fingerprint,
        fingerprintProof: fingerprintPayload?.fingerprintProof,
      })

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
      setNeedRedirect(true)
      mutate()
    } catch (err: any) {
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
      setDisabled(false)
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
      <TextInput
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
        onClose={closeTos}
        onAccept={() => {
          setAccepted(true)
          closeTos()
          void executeLogin()
        }}
      />
      <Anchor fz="xs" className={misc.alignSelfEnd} component={Link} to="/account/recovery">
        {t('account.anchor.recovery')}
      </Anchor>
      <Grid grow w="100%">
        <Grid.Col span={2}>
          <Button fullWidth variant="outline" component={Link} to="/account/register">
            {t('account.button.register')}
          </Button>
        </Grid.Col>
        <Grid.Col span={2}>
          <Button fullWidth disabled={disabled} onClick={onLogin}>
            {t('account.button.login')}
          </Button>
        </Grid.Col>
      </Grid>
      <OAuthButtons />
    </AccountView>
  )
}

export default Login
