import assert from 'node:assert/strict'

// Runs against the real component with the competition harness's read-only API fixtures.
export const auditGlobeFocus = async (cdp, evaluate, waitFor, inspect, name) => {
  const category = await evaluate(`document.querySelector('[data-globe-choice]')?.dataset.globeChoice.startsWith('category-')`)
  if (category) {
    await evaluate(`document.querySelector('[data-globe-choice]').focus()`)
    for (const type of ['keyDown', 'keyUp']) await cdp.send('Input.dispatchKeyEvent', { type, key:'Enter', windowsVirtualKeyCode:13, ...(type === 'keyDown' ? { text:'\r', unmodifiedText:'\r' } : {}) })
    await waitFor(`document.querySelector('[data-globe-choice]') && !document.querySelector('[data-globe-choice]').dataset.globeChoice.startsWith('category-')`)
    assert.equal(await evaluate(`!!document.querySelector('[data-challenge-detail], [role="dialog"]')`), false, 'category selection drills down without opening an arbitrary challenge')
  }
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
    await detail(id)
  }
  const detail = async (id) => {
    await waitFor(`(() => { const panel = document.querySelector('[data-challenge-detail], [role="dialog"]'); const label = document.querySelector('[data-globe-node=' + CSS.escape(${JSON.stringify(id)}) + ']').title; return location.hash.startsWith('#' + ${JSON.stringify(id)} + '-') && panel?.textContent.includes('Challenge files: ' + label + '.') && panel.querySelector('input'); })()`)
    assert.equal(await evaluate(`document.querySelectorAll('[data-globe-choice][aria-pressed="true"]').length`), 1)
    assert.equal(await evaluate(`document.querySelector('[data-globe-choice][aria-pressed="true"]').dataset.globeChoice`), id, 'card, navigator selection and camera agree')
  }
  const close = async () => {
    if (await evaluate(`!!document.querySelector('[role="dialog"]')`)) await press('Escape', 27)
    else await evaluate(`document.querySelector('[data-challenge-detail] button[aria-label="Close"]').click()`)
    await waitFor(`!document.querySelector('[data-challenge-detail], [role="dialog"]')`)
  }
  await focus(first)
  // Desktop can switch directly while details remain open. Mobile closes its modal first.
  if (await evaluate(`!!document.querySelector('[role="dialog"]')`)) await close()
  await focus(last)
  await inspect(`${name}-target-details`)
  await close()
  await waitFor(`document.activeElement.dataset.globeChoice === ${JSON.stringify(last)}`)
  await evaluate(`document.querySelector('[data-globe-stage]').scrollIntoView({ block:'center', behavior:'instant' })`)
  assert.equal(await evaluate(`(() => { const pin = document.querySelector('[data-globe-node][data-focused]'); const p = pin.getBoundingClientRect(); const s = document.querySelector('[data-globe-stage]').getBoundingClientRect(); return p.width > 0 && p.height >= 44 && p.left >= s.left && p.right <= s.right && p.top >= s.top && p.bottom <= s.bottom; })()`), true, 'the centered target is visible and usable on compact horizons too')
  await inspect(`${name}-target-focused`)

  // A newer target wins even when the old focus has not finished.
  await evaluate(`document.querySelector('[data-globe-choice=' + CSS.escape(${JSON.stringify(first)}) + ']').click(); document.querySelector('[data-globe-choice=' + CSS.escape(${JSON.stringify(last)}) + ']').click()`)
  await waitFor(centered(last))
  await detail(last)
  await close()
  // Reset and repeat the same selection: it must re-center, not get stuck on its ID.
  await evaluate(`document.querySelector('[data-globe-stage]').focus({ preventScroll:true })`)
  await press('Home', 36)
  await waitFor(`Number(document.querySelector('[data-globe-stage]').dataset.globeYaw) === 0`)
  await focus(last)
  await close()
  await evaluate(`document.querySelector('[data-globe-node=' + CSS.escape(${JSON.stringify(last)}) + ']').focus()`)
  await press('Enter', 13)
  await detail(last)
  await close()
  await evaluate(`document.querySelector('[data-globe-stage]').focus({ preventScroll:true })`)
  await press('Home', 36)
  await waitFor(`Number(document.querySelector('[data-globe-stage]').dataset.globeYaw) === 0`)
  if (category) {
    // Restore the original category scope for the next independent viewport test.
    await evaluate(`document.querySelector('[data-challenge-category-tabs] [role="tab"]').click()`)
    await waitFor(`document.querySelector('[data-globe-choice]')?.dataset.globeChoice.startsWith('category-')`)
  }
  console.log(`PASS: ${name} one-click card + globe, latest target, repeat selection and focus restoration`)
}
