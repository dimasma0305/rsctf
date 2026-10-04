export interface NormalNoticeDraft {
  content: string
  scheduled: boolean
  publishAt: number | null
}

/** Compare the exact editor-owned draft, including an explicit unschedule. */
export const sameNormalNoticeDraft = (left: NormalNoticeDraft, right: NormalNoticeDraft): boolean =>
  left.content === right.content &&
  left.scheduled === right.scheduled &&
  (!left.scheduled || left.publishAt === right.publishAt)
