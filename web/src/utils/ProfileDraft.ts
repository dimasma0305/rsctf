import type { ProfileUpdateModel, ProfileUserInfoModel } from '../Api'

export const profileFields = (user?: ProfileUpdateModel) => ({
  userName: user?.userName ?? '',
  bio: user?.bio ?? '',
  phone: user?.phone ?? '',
  realName: user?.realName ?? '',
  stdNumber: user?.stdNumber ?? '',
})

export type ProfileFields = ReturnType<typeof profileFields>

export const sameProfile = (left: ProfileFields, right: ProfileFields) =>
  (Object.keys(left) as (keyof ProfileFields)[]).every((key) => left[key] === right[key])

// Avatar/email cache refreshes must not overwrite an unfinished profile edit.
export const refreshedProfileDraft = (
  draft: ProfileFields,
  previous: ProfileUserInfoModel | undefined,
  next: ProfileUserInfoModel | undefined
) => (previous?.userId !== next?.userId || sameProfile(draft, profileFields(previous)) ? profileFields(next) : draft)
