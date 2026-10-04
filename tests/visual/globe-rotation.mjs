import assert from 'node:assert/strict'

// Real browser input against the existing read-only competition fixtures.
export const auditGlobeRotation = async (cdp, evaluate, waitFor, { touch = false, onTilt } = {}) => {
  const turn = Math.PI * 2
  const yaw = () => evaluate(`Number(document.querySelector('[data-globe-stage]').dataset.globeYaw)`)
  const pitch = () => evaluate(`Number(document.querySelector('[data-globe-stage]').dataset.globePitch)`)
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
  for (const key of ['ArrowRight', 'ArrowLeft', 'ArrowUp', 'ArrowDown']) {
    const code = { ArrowRight:39, ArrowLeft:37, ArrowUp:38, ArrowDown:40 }[key]
    const vertical = key === 'ArrowUp' || key === 'ArrowDown'
    for (let i = 1; i <= 24; i++) {
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key, windowsVirtualKeyCode: code })
      await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key, windowsVirtualKeyCode: code })
      await settle()
      closeTo(await (vertical ? pitch() : yaw()), (key === 'ArrowRight' || key === 'ArrowUp' ? 1 : -1) * i * Math.PI / 12, `${key} rotates through every quadrant`)
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
    await evaluate(`(() => { window.__globeInputTrace = []; for (const type of ['pointerdown','pointerup','pointercancel','lostpointercapture']) document.addEventListener(type, e => window.__globeInputTrace.push({ type, x:e.clientX, y:e.clientY, target:e.target.tagName, stage:!!e.target.closest('[data-globe-stage]'), button:!!e.target.closest('button'), id:e.pointerId, primary:e.isPrimary }), true); })()`)
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
    await evaluate(`document.querySelector('[data-globe-stage]').focus({ preventScroll:true })`)
    await cdp.send('Input.dispatchKeyEvent', { type:'keyDown', key:'Home', windowsVirtualKeyCode:36 })
    await cdp.send('Input.dispatchKeyEvent', { type:'keyUp', key:'Home', windowsVirtualKeyCode:36 })
    await settle()
    for (const [dx, dy] of [[0, -rect.height * 0.6], [0, rect.height * 0.6], [rect.width * 0.1, -rect.height * 0.1]]) {
      const previousYaw = await yaw(), previousPitch = await pitch()
      const startX = rect.x + rect.width * 0.05, startY = rect.y + rect.height * (dy < 0 ? 0.8 : 0.2)
      await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x:startX, y:startY, button:'left', clickCount:1 })
      for (let step = 1; step <= 8; step++) {
        await cdp.send('Input.dispatchMouseEvent', { type:'mouseMoved', x:startX + dx * step / 8, y:startY + dy * step / 8, buttons:1 })
      }
      await cdp.send('Input.dispatchMouseEvent', { type:'mouseReleased', x:startX + dx, y:startY + dy, button:'left', clickCount:1 })
      await settle()
      if (Math.abs(Math.sin(await yaw() - previousYaw - dx * Math.PI / width)) > 0.025 || Math.abs(Math.sin(await pitch() - previousPitch + dy * Math.PI / width)) > 0.025) console.log('gesture diagnostics', await evaluate('window.__globeInputTrace'))
      closeTo(await yaw(), previousYaw + dx * Math.PI / width, 'diagonal mouse drag rotates horizontally')
      closeTo(await pitch(), previousPitch - dy * Math.PI / width, 'mouse drag rotates up and down')
      assert.equal(await evaluate(surfaceTop), scroll, 'vertical dragging does not scroll the page')
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
    const previousPitch = await pitch()
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y: touchY }] })
    for (let step = 1; step <= 6; step++) {
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: touchY - step * 12 }] })
    }
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await settle()
    closeTo(await pitch(), previousPitch + 72 * Math.PI / width, 'native vertical touch swipe tilts the globe')
    closeTo(await yaw(), beforeVertical, 'vertical touch swipe leaves horizontal rotation unchanged')
    assert.equal(await evaluate(surfaceTop), scroll, 'vertical globe swipe does not scroll the page')
    // Start clearly outside the surface, beyond Chromium's touch-target expansion.
    const outside = await evaluate(`(() => { const r = document.querySelector('[data-challenge-globe] nav').getBoundingClientRect(); return { x:r.x + r.width / 2, y:r.y + 24 }; })()`)
    await cdp.send('Input.dispatchTouchEvent', { type:'touchStart', touchPoints:[outside] })
    for (let step = 1; step <= 6; step++) {
      await cdp.send('Input.dispatchTouchEvent', { type:'touchMove', touchPoints:[{ x:outside.x, y:outside.y - step * 12 }] })
    }
    await cdp.send('Input.dispatchTouchEvent', { type:'touchEnd', touchPoints:[] })
    await settle()
    closeTo(await pitch(), previousPitch + 72 * Math.PI / width, 'outside touch targets do not capture globe gestures')
    await waitFor(`Math.abs(${surfaceTop} - ${scroll}) > 10`)
    closeTo(await pitch(), previousPitch + 72 * Math.PI / width, 'outside page scrolling does not rotate')
    await cdp.send('Emulation.setTouchEmulationEnabled', { enabled: false })
  }
  const stopped = await yaw()
  const stoppedPitch = await pitch()
  await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y })
  await settle()
  closeTo(await yaw(), stopped, 'released/cancelled gestures do not keep rotating')
  closeTo(await pitch(), stoppedPitch, 'vertical rotation stops on release')
  assert.equal(await evaluate(`(() => { const stage = document.querySelector('[data-globe-stage]').getBoundingClientRect(); return [...document.querySelectorAll('[data-globe-node]:not([hidden])')].filter(node => node.offsetParent).every(node => { const r = node.getBoundingClientRect(); return r.left >= stage.left && r.right <= stage.right && r.top >= stage.top && r.bottom <= stage.bottom; }); })()`), true, 'tilted pins stay inside the horizon crop')
  await onTilt?.()
  assert.equal(await evaluate(`!!document.querySelector('[data-challenge-detail], [role="dialog"]')`), false, 'rotation never opens a challenge')
  await evaluate(`document.querySelector('[data-globe-stage]').focus({ preventScroll:true })`)
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Home', windowsVirtualKeyCode: 36 })
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Home', windowsVirtualKeyCode: 36 })
  await settle()
  closeTo(await yaw(), 0, 'Home restores the original view')
  closeTo(await pitch(), 0, 'Home resets vertical rotation too')
  console.log(`PASS: two-axis globe rotation ${touch ? 'touch + outside page scrolling' : 'mouse + wheel + trackpad'} and keyboard full turns`)
}
