import { computeAccessibleName, getRole, isInaccessible } from 'dom-accessibility-api'

// Bundled only into the test browser, never into the application.
export function scanControls() {
  const controls = []
  const roles = new Set([
    'button',
    'link',
    'tab',
    'menuitem',
    'menuitemcheckbox',
    'menuitemradio',
    'checkbox',
    'radio',
    'switch',
    'combobox',
    'textbox',
    'slider',
    'spinbutton',
    'option',
    'treeitem',
  ])
  const path = (e) => {
    const siblings = [...(e.parentNode?.children || [e])].filter((s) => s.localName === e.localName)
    const own = `${e.localName}:nth-of-type(${siblings.indexOf(e) + 1})`
    return e.parentElement ? `${path(e.parentElement)} > ${own}` : own
  }
  const visit = (root, prefix = '') => {
    for (const e of root.querySelectorAll('*')) {
      if (e.shadowRoot) visit(e.shadowRoot, `${prefix}${path(e)} >>> `)
      const role = getRole(e)
      const props = Object.keys(e).find((k) => k.startsWith('__reactProps$'))
      const handler =
        typeof e.onclick === 'function' ||
        ['onClick', 'onDoubleClick', 'onPointerDown', 'onMouseDown'].some((k) => typeof e[props]?.[k] === 'function')
      const native = e.matches(
        'a[href],area[href],button,input:not([type="hidden"]),select,textarea,summary,label[for]'
      )
      const pointer =
        getComputedStyle(e).cursor === 'pointer' &&
        (!e.parentElement || getComputedStyle(e.parentElement).cursor !== 'pointer')
      if (!native && !roles.has(role) && !handler && !pointer && !e.hasAttribute('tabindex')) continue
      if (e.matches('input[type="hidden"]')) continue
      const rendered = e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden'
      const hidden = !rendered || isInaccessible(e)
      const disabled = e.matches(':disabled') || e.closest('[inert],[aria-disabled="true"]') !== null
      const selector = `${prefix}${path(e)}`
      const label = computeAccessibleName(e)
      const hasPopup = e.getAttribute('aria-haspopup')
      const disclosure = e.localName === 'summary' || e.hasAttribute('aria-expanded')
      const safeOpen =
        !hidden && !disabled && !e.closest('a[href]') && ((hasPopup && hasPopup !== 'false') || disclosure)
      controls.push({
        selector,
        tag: e.localName,
        role: role || null,
        name: label,
        href: e.getAttribute('href'),
        type: e.getAttribute('type'),
        hidden,
        disabled,
        handler,
        pointer,
        hasPopup,
        disclosure,
        safeOpen: Boolean(safeOpen),
        keyboardFocusable: e.tabIndex >= 0,
        status: hidden ? 'hidden' : disabled ? 'disabled' : 'discovered',
        reason: hidden
          ? 'Not exposed in this rendered state'
          : disabled
            ? 'Disabled in this fixture'
            : safeOpen
              ? 'Eligible for open/close probe'
              : 'Needs an explicit behavior scenario',
      })
    }
  }
  visit(document)
  return controls
}

window.__scanControls = scanControls
