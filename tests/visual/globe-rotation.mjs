import assert from 'node:assert/strict'

// Real browser input against the existing read-only competition fixtures.
export const auditGlobeRotation = async (cdp, evaluate, waitFor, { touch = false } = {}) => {
  const turn = Math.PI * 2
  const yaw = () => evaluate(`Number(document.querySelector('[data-globe-stage]').dataset.globeYaw)`)
  // Mobile scrolls the app shell, not window.scrollY. Geometry covers either owner.
  const surfaceTop = `document.querySelector('[data-globe-stage]').getBoundingClientRect().top`
  const settle = () => evaluate(`new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))`)
  const closeTo = (actual, expected, reason) => {
    assert.ok(Math.abs(Math.atan2(Math.sin(actual - expected), Math.cos(actual - expected))) < 0.025, `${reason}: ${actual} vs ${expected}`)
    assert.ok(actual >= 0 && actual < turn, 'orientation stays bounded across full turns')
  }
  await evaluate(`document.querySelector('[data-globe-stage]').scrollIntoView({ block: 'center', behavior: 'instant' }); document.querySelector('[data-globe-stage]').focus({ preventScroll: true })`)
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Home', windowsVirtualKeyCode: 36 })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Home', windowsVirtualKeyCode: 36 })
  await settle()
  const initial = await evaluate(`parseFloat(document.querySelector('[data-globe-node]').style.getPropertyValue('--node-x'))`)
  for (const key of ['ArrowRight', 'ArrowLeft']) {
    for (let i = 1; i <= 24; i++) {
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key, windowsVirtualKeyCode: key === 'ArrowRight' ? 39 : 37 })
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key, windowsVirtualKeyCode: key === 'ArrowRight' ? 39 : 37 })
      await settle()
      closeTo(await yaw(), (key === 'ArrowRight' ? 1 : -1) * i * Math.PI / 12, 'keyboard rotates through every quadrant')
    }
  }
  assert.ok(Math.abs(await evaluate(`parseFloat(document.querySelector('[data-globe-node]').style.getPropertyValue('--node-x'))`) - initial) < 0.001, 'pins return after a full turn')
  assert.equal(await evaluate(`getComputedStyle(document.querySelector('[data-globe-stage]')).outlineStyle`), 'solid')
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Tab', windowsVirtualKeyCode: 9 })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Tab', windowsVirtualKeyCode: 9 })
  assert.equal(await evaluate(`document.activeElement === document.querySelector('[data-globe-stage]')`), false, 'Tab is never trapped')

  const rect = await evaluate(`(() => { const r = document.querySelector('[data-globe-stage]').getBoundingClientRect(); return { x:r.x, y:r.y, width:r.width, height:r.height, clientWidth:document.querySelector('[data-globe-stage]').clientWidth }; })()`)
  const x = rect.x + rect.width * 0.2, y = rect.y + rect.height * 0.97
  const width = Math.max(240, rect.clientWidth)
  if (!touch) {
    const scroll = await evaluate(surfaceTop)
    for (const [deltaX, deltaY] of [[0, width * 2.5], [-width * 3, 0]]) {
      const previous = await yaw()
      await cdp.send('Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX, deltaY })
      await waitFor(`Number(document.querySelector('[data-globe-stage]').dataset.globeYaw) !== ${previous}`)
      await settle()
      closeTo(await yaw(), previous + (deltaX || deltaY) * Math.PI / width, 'wheel and horizontal trackpad scroll cross 360 degrees')
      assert.equal(await evaluate(surfaceTop), scroll, 'rotating the globe does not move the page')
    }
    for (const deltaMode of [1, 2]) {
      const previous = await yaw()
      assert.equal(await evaluate(`document.querySelector('[data-globe-stage]').dispatchEvent(new WheelEvent('wheel', { deltaY:1, deltaMode:${deltaMode}, cancelable:true }))`), false)
      await settle()
      closeTo(await yaw(), previous + Math.PI * (deltaMode === 1 ? 16 : width) / width, 'line/page wheel units are normalized')
    }
    const beforeZoom = await yaw()
    assert.equal(await evaluate(`document.querySelector('[data-globe-stage]').dispatchEvent(new WheelEvent('wheel', { deltaY:100, ctrlKey:true, cancelable:true }))`), true, 'browser zoom is not intercepted')
    await settle()
    closeTo(await yaw(), beforeZoom, 'zoom does not rotate the globe')
    for (let gesture = 0; gesture < 4; gesture++) {
      const previous = await yaw()
      await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x, y, button: 'left', clickCount: 1 })
      for (let step = 1; step <= 8; step++) {
        await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: x + rect.width * 0.6 * step / 8, y, buttons: 1 })
      }
      await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: x + rect.width * 0.6, y, button: 'left', clickCount: 1 })
      await settle()
      closeTo(await yaw(), previous + rect.width * 0.6 * Math.PI / width, 'repeated mouse drags have no rotation stop')
    }
  } else {
    await cdp.send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 2 })
    const previous = await yaw()
    const touchY = rect.y + rect.height * 0.5
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y: touchY }] })
    for (let step = 1; step <= 8; step++) {
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: x + rect.width * 0.6 * step / 8, y: touchY }] })
    }
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await settle()
    closeTo(await yaw(), previous + rect.width * 0.6 * Math.PI / width, 'native horizontal touch swipe rotates')
    const scroll = await evaluate(surfaceTop)
    const beforeVertical = await yaw()
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y: touchY }] })
    for (let step = 1; step <= 6; step++) {
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: touchY - step * 12 }] })
    }
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await waitFor(`Math.abs(${surfaceTop} - ${scroll}) > 10`)
    closeTo(await yaw(), beforeVertical, 'vertical touch scroll does not rotate')
    await cdp.send('Emulation.setTouchEmulationEnabled', { enabled: false })
  }
  const stopped = await yaw()
  await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y })
  await settle()
  closeTo(await yaw(), stopped, 'released/cancelled gestures do not keep rotating')
  assert.equal(await evaluate(`!!document.querySelector('[data-challenge-detail], [role="dialog"]')`), false, 'rotation never opens a challenge')
  await evaluate(`document.querySelector('[data-globe-stage]').focus({ preventScroll:true })`)
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Home', windowsVirtualKeyCode: 36 })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Home', windowsVirtualKeyCode: 36 })
  await settle()
  closeTo(await yaw(), 0, 'Home restores the original view')
  console.log(`PASS: globe rotation ${touch ? 'touch + vertical scrolling' : 'mouse + wheel + trackpad'} and keyboard full turns`)
}
