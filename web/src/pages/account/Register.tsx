import { Anchor, Button, PasswordInput, TextInput } from '@mantine/core'
import { useDisclosure, useInputState } from '@mantine/hooks'
import { showNotification, updateNotification } from '@mantine/notifications'
import { mdiCheck, mdiClose } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { AccountView } from '@Components/AccountView'
import { Captcha, useCaptchaRef } from '@Components/Captcha'
import { OAuthButtons } from '@Components/OAuthButtons'
import { StrengthPasswordInput } from '@Components/StrengthPasswordInput'
import { TermsOfService } from '@Components/TermsOfService'
import { encryptApiData } from '@Utils/Crypto'
import { tryGetClientError } from '@Utils/Shared'
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
  const [accepted, setAccepted] = useState(false)
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

  const executeRegister = async () => {
    if (config.enableBrowserFingerprint && !accepted) {
      openTos()
      return
    }

    if (pwd !== retypedPwd) {
      showNotification({
        color: 'red',
        title: t('common.error.check_input'),
        message: t('account.password.not_match'),
        icon: <Icon path={mdiClose} size={1} />,
      })
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
      id: 'register-status',
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

      const res = await api.account.accountRegister({
        userName: uname,
        password: await encryptApiData(t, pwd, config.apiPublicKey),
        email: email,
        challenge: token,
        fingerprint: fingerprintPayload?.fingerprint,
        fingerprintProof: fingerprintPayload?.fingerprintProof,
        bootstrapToken: bootstrapMode ? bootstrapToken : undefined,
      })
      const data = RegisterStatusMap.get(res.data.data)
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
      setDisabled(false)
    }
  }

  const onRegister = async (event: React.SyntheticEvent) => {
    event.preventDefault()
    await executeRegister()
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
            'Enter the one-time setup token shown by your rsctf installer or Helm notes.',
          )}
          value={bootstrapToken}
          disabled={disabled}
          onChange={(event) => setBootstrapToken(event.currentTarget.value)}
          w="100%"
          autoComplete="off"
        />
      )}
      <TextInput
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
        onClose={closeTos}
        onAccept={() => {
          setAccepted(true)
          closeTos()
          void executeRegister()
        }}
      />
      <Anchor fz="xs" className={misc.alignSelfEnd} component={Link} to="/account/login">
        {t('account.anchor.login')}
      </Anchor>
      <Button type="submit" fullWidth onClick={onRegister} disabled={disabled}>
        {t('account.button.register')}
      </Button>
      <OAuthButtons />
    </AccountView>
  )
}

export default Register
