import assert from 'node:assert/strict'

// The backend can remove a tracked anonymous volume before fixture teardown.
// Only that exact absence is success; permission, in-use, and daemon errors fail.
export async function removeAnonymousVolume(docker, name) {
  assert.match(name, /^[a-f0-9]{64}$/, 'only tracked anonymous volumes may be removed')
  try {
    await docker(['volume', 'rm', name])
    return true
  } catch (error) {
    const message = String(error.stderr || '').trim()
    if (message === `Error response from daemon: get ${name}: no such volume`
      || message === `Error response from daemon: no such volume: ${name}`) return false
    throw error
  }
}
