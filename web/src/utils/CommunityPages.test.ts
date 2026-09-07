import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const posts = readFileSync('src/pages/posts/Index.tsx', 'utf8')
const about = readFileSync('src/pages/About.tsx', 'utf8')
const card = readFileSync('src/components/PostCard.tsx', 'utf8')
const aboutStyles = readFileSync('src/styles/pages/About.module.css', 'utf8')

test('posts retain the requested page until its server total has arrived', () => {
  assert.match(posts, /if \(postPage && activePage > pageCount\)/)
  assert.match(posts, /useInfoGetPostsPage\(pageQuery, OnceSWRConfig\)/)
  assert.doesNotMatch(posts, /posts\?\.slice|setInterval|refreshInterval/)
  assert.match(posts, /pageCount > 1/)
})

test('posts have distinct loading, retry and empty states without blocking the navigation', () => {
  assert.match(posts, /!postPage && !error/)
  assert.match(posts, /role="alert"/)
  assert.match(posts, /onClick=\{\(\) => void mutate\(\)\}/)
  assert.match(posts, /data-posts-loading/)
  assert.match(posts, /animate=\{false\}/)
  assert.match(posts, /posts\?\.length === 0/)
  assert.doesNotMatch(posts, /<WithNavBar isLoading|mih="calc\(100vh/)
})

test('post titles are native links and bylines keep machine-readable dates and readable authors', () => {
  assert.match(card, /<Link to=\{`\/posts\/\$\{post.id\}`\}/)
  assert.match(card, /component="time"/)
  assert.match(card, /dateTime=\{published.isValid\(\) \? published.toISOString\(\)/)
  assert.match(card, /className=\{classes.authorName\}/)
  assert.doesNotMatch(card, /truncate title=\{metadata\}/)
  assert.match(posts, /headingOrder=\{3\}/)
  assert.match(card, /headingOrder = 2/)
  assert.match(card, /<Markdown source=\{post.summary\}/)
})

test('post administration keeps existing role and mutation ownership', () => {
  for (const source of [posts, card]) assert.match(source, /RequireRole\(Role.Admin, role\)/)
  assert.match(posts, /invalidatePostPageCaches\(mutateCache\)/)
  assert.match(posts, /api.edit.editUpdatePost\(post.id/)
  assert.match(card, /disabled=\{disabled\}/)
  assert.match(card, /onTogglePinned\(post, setDisabled\)/)
})

test('about keeps its links and legal notices with unique, static contributor links', () => {
  assert.match(about, /<PageHeader/)
  assert.match(about, /RSCTF_DOCUMENTATION/)
  assert.match(about, /ValidatedRepoMeta\(\)/)
  assert.match(about, /contributorsData.map\(\(contributor\)/)
  assert.match(about, /<ul className=\{classes.contributors\}/)
  assert.match(about, /\/legal\/LICENSING.md/)
  assert.match(about, /\/legal\/third-party\/CreepJS-LICENSE.txt/)
  assert.doesNotMatch(about, /group.concat|setInterval|dangerouslySetInnerHTML/)
  assert.doesNotMatch(aboutStyles, /infinite|height: calc\(100vh/)
  assert.match(aboutStyles, /prefers-reduced-motion/)
  assert.match(aboutStyles, /focus-visible/)
})

test('new community copy has English and Indonesian translations', () => {
  for (const locale of ['en-US', 'id-ID']) {
    const common = JSON.parse(readFileSync('src/locales/' + locale + '/common.json', 'utf8'))
    const post = JSON.parse(readFileSync('src/locales/' + locale + '/post.json', 'utf8'))
    for (const key of ['intro', 'eyebrow', 'guide_description', 'legal_title', 'legal_description', 'revision']) {
      assert.ok(common.content.about[key], locale + ': ' + key)
    }
    for (const key of ['latest', 'load_error', 'loading', 'retry_hint', 'empty_title', 'read_post']) {
      assert.ok(post.content[key], locale + ': ' + key)
    }
  }
})
