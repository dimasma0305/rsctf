/**
 * `navigator.clipboard.writeText` only resolves in **secure contexts**
 * (HTTPS, or HTTP on `localhost`). Plain HTTP on any other host — e.g.
 * `http://1pc.tf:8080` — has two failure modes depending on the browser:
 * <ul>
 *   <li>Older / restricted UAs: `navigator.clipboard === undefined`,
 *       so calls throw synchronously.</li>
 *   <li>Modern Chrome / Firefox: the API exists but `writeText()` always
 *       rejects with <code>NotAllowedError: Write permission denied</code>.</li>
 * </ul>
 * Mantine's <code>useClipboard</code> only guards the first case ("clipboard"
 * in navigator) — it doesn't catch the rejection and fall back. So every
 * <code>CopyButton</code> over plain HTTP silently fails (the green "Copied"
 * flash never fires).
 *
 * Fix: wrap writeText so the native call is tried first; if it rejects
 * or throws, fall back to the legacy <code>document.execCommand('copy')</code>
 * path (stash text in an offscreen textarea, select, copy, tear down).
 * Native API is preferred when it works (async, no DOM churn, respects
 * paste targets); the fallback only triggers when the native one fails.
 *
 * Idempotent — calling multiple times is safe.
 */
export function installClipboardPolyfill(): void {
  if (typeof window === 'undefined' || typeof document === 'undefined') return

  const w = window as any
  if (w.__rsctf_clipboard_polyfill_installed) return
  w.__rsctf_clipboard_polyfill_installed = true

  const fallbackWriteText = (text: string): Promise<void> => {
    return new Promise((resolve, reject) => {
      try {
        const ta = document.createElement('textarea')
        ta.value = text
        // Take the textarea out of the layout flow but keep it focusable
        // (display:none / visibility:hidden would block select()).
        ta.setAttribute('readonly', '')
        ta.style.position = 'fixed'
        ta.style.top = '0'
        ta.style.left = '0'
        ta.style.width = '1px'
        ta.style.height = '1px'
        ta.style.opacity = '0'
        ta.style.pointerEvents = 'none'
        document.body.appendChild(ta)

        // iOS Safari needs an explicit range over the contents; bare .select()
        // doesn't trigger the OS-level copy hook.
        const prevSelection = document.getSelection()?.rangeCount ? document.getSelection()!.getRangeAt(0) : null
        ta.select()
        ta.setSelectionRange(0, text.length)

        const ok = document.execCommand('copy')
        document.body.removeChild(ta)

        // Restore whatever the user had selected before we hijacked focus.
        if (prevSelection) {
          const sel = document.getSelection()
          sel?.removeAllRanges()
          sel?.addRange(prevSelection)
        }

        if (ok) resolve()
        else reject(new Error('execCommand("copy") returned false'))
      } catch (e) {
        reject(e instanceof Error ? e : new Error(String(e)))
      }
    })
  }

  // Try execCommand FIRST, synchronously, inside the user-gesture call
  // stack — that's the only thing that works on plain HTTP, and on
  // HTTPS it also works (just deprecated). Falling through to the
  // async native API would lose the user-gesture context (the .catch
  // handler runs after the gesture event has resolved), so the async
  // path is only reachable if execCommand itself fails (rare —
  // happens in some sandboxed iframes).
  const nativeClipboard = (navigator as any).clipboard
  const nativeWrite =
    nativeClipboard && typeof nativeClipboard.writeText === 'function'
      ? nativeClipboard.writeText.bind(nativeClipboard)
      : null

  const writeText = (text: string): Promise<void> => {
    return fallbackWriteText(text).catch((e) => {
      if (nativeWrite) return nativeWrite(text)
      throw e
    })
  }

  const clip = nativeClipboard ?? {}
  clip.writeText = writeText
  try {
    Object.defineProperty(navigator, 'clipboard', { value: clip, configurable: true })
  } catch {
    ;(navigator as any).clipboard = clip
  }
}
