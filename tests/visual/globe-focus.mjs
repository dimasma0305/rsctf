import assert from 'node:assert/strict'

// Runs against the real component with the competition harness's read-only API fixtures.
export const auditGlobeFocus = async (cdp, evaluate, waitFor, inspect, name) => {
  const ids = await evaluate(`[...document.querySelectorAll('[data-globe-choice]')].map(node => node.dataset.globeChoice)`)
  assert.ok(ids.length > 0)
  const first = ids[0], last = ids.at(-1)
  const centered = (id) => `(() => {
    const pin = document.querySelector('[data-globe-node=' + CSS.escape(${JSON.stringify(id)}) + ']');
    return pin?.dataset.focused && !pin.hidden && Math.abs(parseFloat(pin.style.getPropertyValue('--node-x')) - 50) < 0.01 && Math.abs(parseFloat(pin.style.getPropertyValue('--node-y')) - 29.36) < 0.01;
  })()`
  const press = async (key, code) => {
    for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key, windowsVirtualKeyCode:code, ...(type === 'keyDown' && key === 'Enter' ? { text:'\r', unmodifiedText:'\r' } : {}) })
  }
  const focus = async (id) => {
    await evaluate(`document.querySelector('[data-globe-choice=' + CSS.escape(${JSON.stringify(id)}) + ']').focus()`)
    await press('Enter', 13)
    await waitFor(centered(id))
    assert.equal(await evaluate(`document.activeElement.dataset.globeChoice`), id, 'camera focus never steals keyboard focus')
    assert.equal(await evaluate(`!!document.querySelector('[data-challenge-detail], [role="dialog"]')`), false, 'focusing does not open a challenge')
  }
  await focus(first)
  await focus(last)
  assert.equal(await evaluate(`document.querySelectorAll('[data-globe-choice][aria-pressed="true"]').length`), 1)
  assert.deepEqual(await evaluate(`[...document.querySelectorAll('[data-globe-choice]')].map(node => node.dataset.globeChoice)`), ids, 'category focus keeps the category globe in place')
  await evaluate(`document.querySelector('[data-globe-stage]').scrollIntoView({ block:'center', behavior:'instant' })`)
  assert.equal(await evaluate(`(() => { const pin = document.querySelector('[data-globe-node][data-focused]'); const p = pin.getBoundingClientRect(); const s = document.querySelector('[data-globe-stage]').getBoundingClientRect(); return p.width > 0 && p.height >= 44 && p.left >= s.left && p.right <= s.right && p.top >= s.top && p.bottom <= s.bottom; })()`), true, 'the centered target is visible and usable on compact horizons too')
  await inspect(`${name}-target-focused`)

  // A newer target wins even when the old focus has not finished.
  await evaluate(`document.querySelector('[data-globe-choice=' + CSS.escape(${JSON.stringify(first)}) + ']').click(); document.querySelector('[data-globe-choice=' + CSS.escape(${JSON.stringify(last)}) + ']').click()`)
  await waitFor(centered(last))
  // Reset and repeat the same selection: it must re-center, not get stuck on its ID.
  await evaluate(`document.querySelector('[data-globe-stage]').focus({ preventScroll:true })`)
  await press('Home', 36)
  await waitFor(`Number(document.querySelector('[data-globe-stage]').dataset.globeYaw) === 0`)
  await focus(last)
  await evaluate(`document.querySelector('[data-globe-stage]').focus({ preventScroll:true })`)
  await press('Home', 36)
  await waitFor(`Number(document.querySelector('[data-globe-stage]').dataset.globeYaw) === 0`)
  console.log(`PASS: ${name} navigator focus, latest target, repeat selection and keyboard ownership`)
}
