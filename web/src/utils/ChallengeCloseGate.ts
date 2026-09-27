/**
 * Why a challenge dialog refuses to close. The fresh-solve review comes first,
 * then any caller-owned requirement (for example a required disclosure).
 */
export type ChallengeCloseBlock = 'review' | 'requirement' | 'both' | null

export const challengeCloseBlock = (reviewPending: boolean, requirementPending: boolean): ChallengeCloseBlock =>
  reviewPending ? (requirementPending ? 'both' : 'review') : requirementPending ? 'requirement' : null
