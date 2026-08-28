import { Alert, Anchor, Button, PasswordInput, Text, TextInput } from '@mantine/core'
import { useDisclosure, useInputState } from '@mantine/hooks'
import { showNotification, updateNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { Captcha, useCaptchaRef } from '@Components/Captcha'
import { OAuthButtons } from '@Components/OAuthButtons'
import { StrengthPasswordInput } from '@Components/StrengthPasswordInput'
import { TermsOfService } from '@Components/TermsOfService'
import {
  AccountMailOperation,
  clearAccountMailOperation,
  retainAccountMailOperation,
} from '@Utils/AccountMailOperations'
import { encryptApiData } from '@Utils/Crypto'
import { collectFingerprintIdentity } from '@Utils/FingerprintIdentity'
import { isAbortError, throwIfAborted } from '@Utils/FingerprintProbe'
import { isRetryableHttpError } from '@Utils/HttpError'
import { tryGetClientError } from '@Utils/Shared'
import { useConsentSingleFlight } from '@Utils/SingleFlightOperation'
import { useConfig } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'
import api, { RegisterStatus } from '@Api'
import misc from '@Styles/Misc.module.css'

const Register: FC = () => {
  const [pwd, setPwd] = useInputState('')
  const [retypedPwd, setRetypedPwd] = useInputState('')
  const [uname, setUname] = useInputState('')
  const [email, setEmail] = useInputState('')
  const [bootstrapToken, setBootstrapToken] = useInputState('')
  const [disabled, setDisabled] = useState(false)
  const mailOperation = useRef<AccountMailOperation | null>(null)
  const [tosOpened, { open: openTos, close: closeTos }] = useDisclosure(false)
  const { config } = useConfig()

  const navigate = useNavigate()
  const [searchParams] = useSearchParams()
  const bootstrapMode = searchParams.get('bootstrap') === '1'
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

  usePageTitle(t('account.title.register'))

  const executeRegister = async (signal: AbortSignal, consentGranted: boolean) => {
    if (pwd !== retypedPwd) {
      showNotification({
        color: 'red',
        title: t('common.error.check_input'),
        message: t('account.password.not_match'),
        icon: <Icon path={mdiClose} size={1} />,
      })
      return
    }

    const mailScope = `${bootstrapMode ? 'bootstrap' : 'public'}\0${email.trim().toLowerCase()}\0${uname
      .trim()
      .toLowerCase()}`
    const owner = retainAccountMailOperation(sessionStorage, 'registration', mailScope, mailOperation.current)
    mailOperation.current = owner

    setDisabled(true)

    try {
      if (config.enableBrowserFingerprint && !consentGranted) {
        throw new Error('Device verification consent is required.')
      }
      const { valid, token } = await getToken()
      throwIfAborted(signal)

      if (!valid) {
        clearAccountMailOperation(sessionStorage, owner)
        mailOperation.current = null
        showNotification({
          color: 'orange',
          title: t('account.notification.captcha.not_valid'),
          message: t('common.error.try_later'),
          loading: true,
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

      const fingerprintPayload = await collectFingerprintIdentity({
        enabled: config.enableBrowserFingerprint,
        apiPublicKey: config.apiPublicKey,
        signal,
        translate: t,
      })
      const password = await encryptApiData(t, pwd, config.apiPublicKey)
      throwIfAborted(signal)

      const res = await api.account.accountRegister(
        {
          userName: uname,
          password,
          email: email,
          challenge: token,
          fingerprint: fingerprintPayload.fingerprint,
          fingerprintProof: fingerprintPayload.fingerprintProof,
          bootstrapToken: bootstrapMode ? bootstrapToken : undefined,
          operationId: owner.operationId,
        },
        { signal }
      )
      throwIfAborted(signal)
      const data = RegisterStatusMap.get(res.data.data)
      clearAccountMailOperation(sessionStorage, owner)
      mailOperation.current = null
      if (data) {
        updateNotification({
          id: 'register-status',
          color: 'teal',
          title: data.title,
          message: data.message,
          icon: <Icon path={mdiCheck} size={1} />,
          loading: false,
          autoClose: true,
        })
        cleanUp(true)

        if (res.data.data === RegisterStatus.LoggedIn) navigate('/')
        else if (res.data.data === RegisterStatus.EmailConfirmationRequired)
          navigate('/account/pending', { state: { email } })
        else navigate('/account/login')
      }
    } catch (err: any) {
      if (signal.aborted || isAbortError(err)) return
      if (!isRetryableHttpError(err)) {
        clearAccountMailOperation(sessionStorage, owner)
        mailOperation.current = null
      }
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
      if (!signal.aborted) setDisabled(false)
    }
  }

  const registerOperation = useConsentSingleFlight({
    requiresConsent: Boolean(config.enableBrowserFingerprint),
    onConsentRequired: openTos,
    operation: executeRegister,
  })

  const onRegister = (event: React.SubmitEvent<HTMLFormElement>) => {
    event.preventDefault()
    void registerOperation.run().catch((error) => {
      if (!isAbortError(error)) {
        const { title, message } = tryGetClientError(error, t)
        showNotification({ color: 'red', title, message, icon: <Icon path={mdiClose} size={1} /> })
      }
    })
  }

  if (!bootstrapMode && config.allowPasswordRegistration === false) {
    const providerAvailable = Boolean(config.enableGoogleAuth || config.enableDiscordAuth)
    return (
      <AccountView title={t('account.title.register')} description={t('account.oauth.registration_only_description')}>
        <Alert color={providerAvailable ? 'blue' : 'red'}>
          <Text size="sm">
            {t(providerAvailable ? 'account.oauth.registration_only_notice' : 'account.oauth.registration_unavailable')}
          </Text>
        </Alert>
        {providerAvailable && <OAuthButtons />}
        <Anchor fz="xs" className={misc.alignSelfEnd} component={Link} to="/account/login">
          {t('account.anchor.login')}
        </Anchor>
      </AccountView>
    )
  }

  return (
    <AccountView
      title={t('account.title.register')}
      description={t('account.content.register.description', 'Create an account and get ready for the next challenge.')}
      onSubmit={onRegister}
    >
      {bootstrapMode && (
        <PasswordInput
          required
          label={t('account.label.bootstrap_token', 'Setup token')}
          description={t(
            'account.content.register.bootstrap_token',
            'Enter the one-time setup token shown by your rsctf installer or Helm notes.'
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
      <StrengthPasswordInput value={pwd} onChange={(event) => setPwd(event.currentTarget.value)} disabled={disabled} />
      <PasswordInput
        required
        label={t('account.label.password_retype')}
        value={retypedPwd}
        onChange={(event) => setRetypedPwd(event.currentTarget.value)}
        disabled={disabled}
        w="100%"
        error={retypedPwd.length > 0 && pwd !== retypedPwd && t('account.password.not_match')}
      />
      <Captcha action="register" ref={captchaRef} />
      <TermsOfService
        confirmMode
        opened={tosOpened}
        onClose={() => {
          registerOperation.rejectConsent()
          closeTos()
        }}
        onAccept={() => {
          registerOperation.acceptConsent()
          closeTos()
        }}
      />
      <Anchor fz="xs" className={misc.alignSelfEnd} component={Link} to="/account/login">
        {t('account.anchor.login')}
      </Anchor>
      <Button type="submit" fullWidth disabled={disabled}>
        {t('account.button.register')}
      </Button>
      <OAuthButtons />
    </AccountView>
  )
}

export default Register
