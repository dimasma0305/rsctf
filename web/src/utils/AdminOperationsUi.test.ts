import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import type { ChallengeBuildAuditModel } from '../Api'
import { matchesBuildQuery } from '../components/admin/builds/buildPresentation'

test('build search matches loaded metadata case-insensitively without depending on optional fields', () => {
  const build = {
    challengeTitle: 'Tower of Babel',
    gameId: 19,
    challengeId: 72,
    imageRef: null,
    kind: 'Checker',
    trigger: 'Manual',
  } as ChallengeBuildAuditModel
  for (const query of ['', '   ', ' TOWER ', '19', '72', 'Checker', 'Manual'])
    assert.ok(matchesBuildQuery(build, query), query)
  assert.equal(matchesBuildQuery(build, 'not-present'), false)
  assert.equal(matchesBuildQuery(build, 'null'), false)
  assert.equal(matchesBuildQuery({ ...build, imageRef: 'rsctf/tower:latest' }, 'tower:latest'), true)
})

test('operation pages use one admin heading and non-blocking data recovery', () => {
  for (const file of ['repo-bindings', 'builds']) {
    const source = readFileSync('src/pages/admin/' + file + '.tsx', 'utf8')
    assert.doesNotMatch(source, /<AdminPage isLoading=/)
    assert.match(source, /<Skeleton[^>]*animate=\{false\}/)
    assert.match(source, /role="alert"/)
    assert.doesNotMatch(source, /<Title order=\{2\}>\{t\('admin\.content\.(?:builds|repo_binding)\.title'/)
  }
  const builds = readFileSync('src/pages/admin/builds.tsx', 'utf8')
  assert.match(builds, /historyError &&/)
  assert.match(builds, /inProgressQuery.error/)
  assert.match(builds, /matchesBuildQuery\(b, search\)/)
  assert.match(builds, /count: 200/)
  assert.match(builds, /disabled=\{!b.logTail && !b.errorMessage && !b.imageRef\}/)
  assert.match(builds, /<AccessibleModal/)
  assert.match(builds, /className=\{ops.log\}/)
})

test('repository creation is explicit, keyboard-submittable and retains one in-flight owner', () => {
  const repo = readFileSync('src/pages/admin/repo-bindings.tsx', 'utf8')
  assert.match(repo, /opened=\{addOpened\}/)
  assert.match(repo, /<form[\s\S]*?onSubmit=/)
  assert.match(repo, /addInFlight.current = true/)
  assert.match(repo, /addInFlight.current = false/)
  assert.match(repo, /setGithubToken\(''\)/)
  assert.match(repo, /type="submit"/)
  assert.match(repo, /closeOnEscape=\{!busy\}/)
  assert.match(repo, /onClick=\{\(\) => onOpenHistory\(b\)\}/)
  assert.match(repo, /<Code\s+block\s+role="region"\s+tabIndex=\{0\}/)
})

test('inventory errors cannot masquerade as an empty inventory', () => {
  const images = readFileSync('src/components/admin/BuildImagesPanel.tsx', 'utf8')
  assert.match(images, /imageError && !images \? null/)
  assert.match(images, /storageQuery.error &&/)
  assert.match(images, /viewportProps=\{\{[\s\S]*?tabIndex: 0/)
})

test('operations copy has matching English and Indonesian translations', () => {
  const en = JSON.parse(readFileSync('src/locales/en-US/admin.json', 'utf8')).operations
  const id = JSON.parse(readFileSync('src/locales/id-ID/admin.json', 'utf8')).operations
  assert.deepEqual(Object.keys(en).sort(), Object.keys(id).sort())
  assert.ok(Object.values(en).every((value) => typeof value === 'string' && value.length))
  assert.ok(Object.values(id).every((value) => typeof value === 'string' && value.length))
})

test('build status labels and global cleanup scope are explicit in both locales', () => {
  for (const locale of ['en-US', 'id-ID']) {
    const builds = JSON.parse(readFileSync(`src/locales/${locale}/admin.json`, 'utf8')).content.builds
    assert.deepEqual(
      Object.keys(builds.status).sort(),
      ['None', 'Success', 'Failed', 'Building', 'Queued', 'MissingDockerfile', 'NotApplicable'].sort()
    )
    assert.match(builds.confirm_prune_failed, /\{\{count\}\}/)
  }
  const en = JSON.parse(readFileSync('src/locales/en-US/admin.json', 'utf8')).content.builds
  assert.match(en.confirm_prune_failed, /older records outside this view/)
})
