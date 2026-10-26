const failureMessages: Readonly<Record<string, string>> = {
  oauth_await_approval: 'await_approval',
  oauth_register_disabled: 'register_disabled',
  oauth_email_unverified: 'email_unverified',
  oauth_email_conflict: 'email_conflict',
  oauth_no_email: 'no_email',
  oauth_email_domain: 'email_domain',
  oauth_account_disabled: 'account_disabled',
  oauth_anti_cheat: 'anti_cheat',
  oauth_state_mismatch: 'state_mismatch',
  oauth_state_expired: 'state_expired',
  oauth_invalid_callback: 'invalid_callback',
  oauth_not_configured: 'not_configured',
  oauth_token_exchange: 'provider_unavailable',
  oauth_userinfo: 'provider_unavailable',
  oauth_policy: 'policy',
  oauth_failed: 'generic',
}

/** Untrusted query values select only allowlisted, translated guidance. */
export const oauthFailure = (code: string | null) => {
  if (!code?.startsWith('oauth_')) return null
  return {
    messageKey: `account.oauth.error.${failureMessages[code] ?? 'generic'}`,
    pending: code === 'oauth_await_approval',
  }
}
