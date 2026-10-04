const TEAM_INVITE_CODE_PATTERN = /:\d+:[0-9a-f]{32}$/

export const isValidTeamInviteCode = (value: string) => TEAM_INVITE_CODE_PATTERN.test(value)
