import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const api = readFileSync('src/Api.ts', 'utf8')
const adOps = readFileSync('src/pages/admin/games/[id]/AdOps.tsx', 'utf8')
const newsPage = readFileSync('src/pages/posts/Index.tsx', 'utf8')

const contractSection = (start: string, end: string) => {
  const startIndex = api.indexOf(start)
  const endIndex = api.indexOf(end, startIndex)
  assert.notEqual(startIndex, -1, `missing API contract start: ${start}`)
  assert.notEqual(endIndex, -1, `missing API contract end: ${end}`)
  return api.slice(startIndex, endIndex)
}

test('legacy posts preserve the complete raw-array API while bounded consumers use pages', () => {
  const legacy = contractSection('infoGetPosts: (', 'infoGetPostsPage: (')
  const page = contractSection('infoGetPostsPage: (', 'infoPowChallenge: (')

  assert.match(legacy, /this\.request<PostInfoModel\[\], any>/)
  assert.match(legacy, /useSWR<PostInfoModel\[\], any>/)
  assert.doesNotMatch(legacy, /ArrayResponseOfPostInfoModel/)

  assert.match(page, /this\.request<ArrayResponseOfPostInfoModel, any>/)
  assert.match(page, /useSWR<ArrayResponseOfPostInfoModel, any>/)
  assert.match(newsPage, /useInfoGetPostsPage\(/)
  assert.doesNotMatch(newsPage, /useInfoGetPosts\(/)
})

test('A&D container reconcile supplies a retained idempotency key', () => {
  const ensure = contractSection('editAdEnsureContainers: (', 'editAdToggleScoringPause: (')

  assert.match(ensure, /operationIdOrParams\?: string \| RequestParams/)
  assert.match(ensure, /\.\.\.requestParams\.headers/)
  assert.match(ensure, /"Idempotency-Key": operationId/)
  assert.match(adOps, /new RetryableOperationKey\(undefined, `rsctf:ad-ensure-containers:\$\{numId\}`\)/)
  assert.match(adOps, /useEffect\(\(\) => \(\) => ensureContainersOwner\.release\(\), \[ensureContainersOwner\]\)/)
  assert.match(adOps, /const operationId = operationOwner\.claim\(\)/)
  assert.match(adOps, /editAdEnsureContainers\(numId, operationId\)/)
  assert.match(adOps, /await api\.edit\.editAdEnsureContainers[\s\S]*operationOwner\.complete\(operationId\)/)
  assert.match(adOps, /httpErrorStatus\(e\) === 409\) operationOwner\.complete\(operationId\)/)
})
