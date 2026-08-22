export const ADMIN_BUILD_IMAGES_CACHE_KEY = '/api/admin/builds/images'
export const ADMIN_BUILD_STORAGE_CACHE_KEY = '/api/admin/builds/storage'

type CacheMutator = (key: string) => Promise<unknown> | unknown

export const refreshAdminBuildImageViews = async (mutate: CacheMutator): Promise<void> => {
  await Promise.all([mutate(ADMIN_BUILD_IMAGES_CACHE_KEY), mutate(ADMIN_BUILD_STORAGE_CACHE_KEY)])
}
