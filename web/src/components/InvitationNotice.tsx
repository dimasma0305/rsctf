import { Alert } from '@mantine/core'
import { useTranslation } from 'react-i18next'
import { isTeamInviteDestination } from '@Utils/TeamInvite'

export function InvitationNotice({ from }: { from: string }) {
  const { t } = useTranslation()
  return isTeamInviteDestination(from) ? (
    <Alert>
      {t(
        'team.invitation.sign_in',
        'You have a team invitation. Sign in or create an account, then confirm Join. Your invitation is saved in this link.'
      )}
    </Alert>
  ) : null
}
