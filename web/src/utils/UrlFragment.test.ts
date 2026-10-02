import assert from 'node:assert/strict'
import test from 'node:test'
import { parseUrlFragment, updateUrlFragment } from './UrlFragment'

test('fragment parameters coexist with legacy anchors and challenge title slugs', () => {
  for (const anchor of ['7', '7-Title-%26-%23', 'anchor']) {
    const hash = updateUrlFragment(`#${anchor}`, { section: 'abnormal-solves' })
    assert.equal(hash, `#${anchor}&section=abnormal-solves`)
    assert.equal(parseUrlFragment(hash).anchor, anchor)
    assert.equal(parseUrlFragment(hash).params.get('section'), 'abnormal-solves')
    assert.equal(updateUrlFragment(hash, { section: null }), `#${anchor}`)
  }
})

test('fragment edits preserve nested snapshot selection, encoded file paths and tab state', () => {
  const hash = '#view=ad&snapshot=2&file=%2Fetc%2Fa%23b%26c%3Dd&snapshotTab=history'
  const changed = updateUrlFragment(hash, { snapshotTab: 'changes' })
  assert.equal(parseUrlFragment(changed).params.get('file'), '/etc/a#b&c=d')
  assert.equal(parseUrlFragment(changed).params.get('snapshot'), '2')
  assert.equal(updateUrlFragment(changed, { snapshot: null, file: null }), '#view=ad&snapshotTab=changes')
  assert.equal(updateUrlFragment('', { section: 'security' }), '#section=security')
  assert.equal(updateUrlFragment('#section=security', { section: null }), '')
})
