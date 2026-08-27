import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const api = readFileSync('src/Api.ts', 'utf8')
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
