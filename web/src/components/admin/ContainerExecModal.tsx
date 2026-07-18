import { Alert, Group, Modal, ModalProps, SegmentedControl, Stack, Text } from '@mantine/core'
import { HubConnection, HubConnectionBuilder } from '@microsoft/signalr'
import { FitAddon } from '@xterm/addon-fit'
import { Terminal } from '@xterm/xterm'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import '@xterm/xterm/css/xterm.css'

interface ContainerExecModalProps extends Omit<ModalProps, 'children'> {
  containerGuid?: string | null
  containerTitle?: string
}

/**
 * In-browser terminal over the ContainerExecHub SignalR endpoint.
 * On mount: build a HubConnection, invoke Open(guid, shell) to get a
 * session id, subscribe to the Stream IAsyncEnumerable for stdout
 * bytes (base64-encoded over JSON), and pipe Terminal.onData back to
 * the server's Input method (also base64).
 * On unmount: invoke Close(sid) so the docker exec dies immediately
 * instead of waiting for the connection timeout.
 *
 * The `shell` state is intentionally NOT a useEffect dependency —
 * toggling the segmented control after a failed connection shouldn't
 * tear down & rebuild the hub. Pick the shell BEFORE clicking Open;
 * to switch, close the modal and reopen.
 */
export const ContainerExecModal: FC<ContainerExecModalProps> = (props) => {
  const { containerGuid, containerTitle, opened, onClose, ...rest } = props
  const { t } = useTranslation()
  // Callback-ref so the effect below re-fires *after* the DOM node is
  // actually attached. A plain useRef misses the first attach because
  // Mantine's Modal portal can mount the children on the same render
  // cycle as the effect — the ref's current is still null when the
  // effect first runs and the connect path was being skipped silently.
  const [terminalEl, setTerminalEl] = useState<HTMLDivElement | null>(null)
  const fitRef = useRef<FitAddon | null>(null)
  const hubRef = useRef<HubConnection | null>(null)
  const sessionIdRef = useRef<string | null>(null)
  const shellRef = useRef<'sh' | 'bash'>('sh')

  const [shell, setShell] = useState<'sh' | 'bash'>('sh')
  const [status, setStatus] = useState<'idle' | 'connecting' | 'connected' | 'closed' | 'error'>('idle')
  const [errorMsg, setErrorMsg] = useState<string | null>(null)

  // The async Clipboard API (and JS-driven paste) only works in a secure
  // context — HTTPS or localhost. On plain HTTP the hint must not promise
  // shortcuts that can't work, so paste is "Ctrl+V / right-click" (native
  // paste event) only, and copy is select-to-copy (execCommand fallback).
  const secureCtx = typeof window !== 'undefined' && window.isSecureContext

  // SignalR JSON encodes byte chunks as base64 strings.
  const decodeBase64 = (s: string): Uint8Array => {
    const raw = atob(s)
    const out = new Uint8Array(raw.length)
    for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i)
    return out
  }

  const encodeBase64 = (bytes: Uint8Array): string => {
    let bin = ''
    for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i])
    return btoa(bin)
  }

  useEffect(() => {
    if (!opened || !containerGuid || !terminalEl) return
    let disposed = false
    shellRef.current = shell

    const term = new Terminal({
      fontFamily: 'JetBrains Mono, Consolas, monospace',
      fontSize: 13,
      cursorBlink: true,
      scrollback: 5000,
      theme: { background: '#0c0c14' },
    })
    const fit = new FitAddon()
    term.loadAddon(fit)
    term.open(terminalEl)
    fitRef.current = fit
    // Defer the first fit until the modal has actually laid out. Fitting
    // synchronously here (during the open animation) sizes the PTY wrong, so
    // the shell wraps/garbles until the next resize. Double-rAF ≈ post-layout.
    const fitNow = () => {
      try {
        fit.fit()
      } catch {
        /* element not sized yet */
      }
    }
    requestAnimationFrame(() => requestAnimationFrame(fitNow))

    // Copy / paste, the way a normal terminal behaves. xterm sends Ctrl+C to
    // the shell (SIGINT) by default and never copies, so a selection + Ctrl+C
    // just kills your command. Wire up collision-free shortcuts:
    //   - Ctrl/⌘+C  → copy IF there's a selection, else fall through to SIGINT
    //   - right-click → copy the selection, or paste when nothing is selected
    //   - Ctrl+Insert copy / Shift+Insert / Ctrl+Shift+V paste (extras)
    //   - plain Ctrl+V uses xterm's built-in paste (works on HTTP too)
    // NOT Ctrl+Shift+C: Chrome reserves it for DevTools "inspect element" at the
    // browser level — preventDefault can't cancel it — so binding copy there
    // just pops DevTools. writeText is polyfilled (installClipboardPolyfill) to
    // fall back to execCommand so copy works over plain HTTP, not just HTTPS.
    const copySelection = (): boolean => {
      const sel = term.getSelection()
      if (!sel) return false
      void navigator.clipboard.writeText(sel).catch(() => undefined)
      return true
    }
    const pasteFromClipboard = () => {
      navigator.clipboard
        ?.readText?.()
        .then((txt) => txt && term.paste(txt))
        .catch(() => undefined) // HTTP: use plain Ctrl+V / right-click instead
    }
    // Swallow a shortcut completely: stop xterm AND the browser default
    // (returning false alone leaves the browser to act — e.g. Ctrl+Shift+C
    // would still open DevTools).
    const swallow = (e: KeyboardEvent) => {
      e.preventDefault()
      e.stopPropagation()
      return false
    }
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== 'keydown') return true
      const mod = e.ctrlKey || e.metaKey
      const key = e.key.toLowerCase()

      // Explicit copy: Ctrl+Insert (Ctrl+Shift+C is browser-reserved, skip it).
      if (e.ctrlKey && e.key === 'Insert') {
        copySelection()
        return swallow(e)
      }
      // Smart Ctrl/⌘+C: copy if there's a selection, else let it through (SIGINT).
      if (mod && !e.shiftKey && key === 'c' && term.hasSelection()) {
        copySelection()
        return swallow(e)
      }
      // Explicit paste: Ctrl+Shift+V / Shift+Insert (async clipboard; secure ctx).
      if ((mod && e.shiftKey && key === 'v') || (e.shiftKey && e.key === 'Insert')) {
        pasteFromClipboard()
        return swallow(e)
      }
      return true
    })

    // Right-click = copy the selection (works on HTTP via the writeText
    // polyfill), or paste when nothing is selected. With no selection on plain
    // HTTP we can't read the clipboard from JS, so we let the browser's native
    // context menu through — its "Paste" still works there.
    const onContextMenu = (ev: MouseEvent) => {
      if (term.hasSelection()) {
        ev.preventDefault()
        copySelection()
        term.clearSelection()
      } else if (navigator.clipboard?.readText) {
        ev.preventDefault()
        navigator.clipboard
          .readText()
          .then((txt) => txt && term.paste(txt))
          .catch(() => undefined)
      }
    }
    terminalEl.addEventListener('contextmenu', onContextMenu)

    // Copy-on-select (PuTTY / tmux style): finishing a drag-selection copies it
    // immediately, so copy works with ZERO shortcuts — important on plain HTTP
    // where navigator.clipboard is unavailable and writeText falls back to
    // execCommand (which needs this user-gesture stack). Refocus so typing
    // continues; xterm's selection is canvas-rendered, so it stays highlighted.
    const onMouseUp = () => {
      if (!term.hasSelection()) return
      copySelection()
      term.focus()
    }
    terminalEl.addEventListener('mouseup', onMouseUp)

    const hub = new HubConnectionBuilder().withUrl('/hub/containerExec').withAutomaticReconnect().build()
    hubRef.current = hub

    // Server pushes terminal output via the "Receive" client method
    // (sessionId, base64Chunk) and signals end-of-session via "Closed"
    // (sessionId, reason). We register the handlers BEFORE invoking
    // Open so we don't drop the welcome chunk that the hub sends
    // immediately after the session opens.
    hub.on('Receive', (sid: string, chunk: string) => {
      if (disposed || sessionIdRef.current !== sid) return
      try {
        term.write(decodeBase64(chunk))
      } catch {
        /* ignore malformed chunk */
      }
    })
    hub.on('Closed', (sid: string, reason: string) => {
      if (disposed || sessionIdRef.current !== sid) return
      setStatus('closed')
      if (reason && reason !== 'eof') setErrorMsg(reason)
    })

    // term -> server, registered ONCE; each handler reads the live session id,
    // so it keeps working across a reconnect that swaps the session underneath
    // (and no-ops before connect / while reconnecting, when the id is null).
    term.onData((data) => {
      const sid = sessionIdRef.current
      if (!sid) return
      hub.invoke('Input', sid, encodeBase64(new TextEncoder().encode(data))).catch(() => undefined)
    })
    term.onResize(({ cols, rows }) => {
      const sid = sessionIdRef.current
      if (!sid) return
      hub.invoke('Resize', sid, cols, rows).catch(() => undefined)
    })

    // Open (or re-open) the exec session. The server's PTY can't survive a
    // transport drop — OnDisconnected disposes it — so on reconnect we spawn a
    // FRESH shell rather than leaving a "connected"-looking but dead terminal
    // that silently swallows keystrokes.
    const openSession = async () => {
      const sid = await hub.invoke<string>('Open', containerGuid, shellRef.current)
      if (disposed) {
        await hub.invoke('Close', sid).catch(() => undefined)
        return
      }
      sessionIdRef.current = sid
      setStatus('connected')
      term.focus()
      fitNow()
      hub.invoke('Resize', sid, term.cols, term.rows).catch(() => undefined)
    }

    // While reconnecting, drop the stale id so input isn't posted into the void.
    hub.onreconnecting(() => {
      sessionIdRef.current = null
      if (!disposed) setStatus('connecting')
    })
    hub.onreconnected(() => {
      if (disposed) return
      term.write('\r\n\x1b[33m[rsctf] reconnected — new shell\x1b[0m\r\n')
      openSession().catch((e) => {
        if (!disposed) {
          setStatus('error')
          setErrorMsg((e as Error).message)
        }
      })
    })
    hub.onclose(() => {
      sessionIdRef.current = null
      if (!disposed) setStatus((s) => (s === 'error' ? s : 'closed'))
    })

    const start = async () => {
      setStatus('connecting')
      setErrorMsg(null)
      try {
        await hub.start()
        if (disposed) return
        await openSession()
      } catch (e) {
        if (!disposed) {
          setStatus('error')
          setErrorMsg((e as Error).message)
        }
      }
    }

    void start()

    // Refit on any size change of the terminal box (modal resize, viewport
    // change), not just window resize — keeps cols/rows correct so the shell
    // wraps properly.
    // Debounce resize-driven refits (~100ms) so a rapid modal/viewport change
    // doesn't thrash the PTY size; term.onResize then pushes the new cols/rows
    // to the pod.
    let fitTimer: ReturnType<typeof setTimeout> | undefined
    const refit = () => {
      clearTimeout(fitTimer)
      fitTimer = setTimeout(fitNow, 100)
    }
    window.addEventListener('resize', refit)
    const ro = new ResizeObserver(refit)
    ro.observe(terminalEl)

    return () => {
      disposed = true
      clearTimeout(fitTimer)
      window.removeEventListener('resize', refit)
      ro.disconnect()
      terminalEl.removeEventListener('contextmenu', onContextMenu)
      terminalEl.removeEventListener('mouseup', onMouseUp)
      const sid = sessionIdRef.current
      const ref = hubRef.current
      sessionIdRef.current = null
      hubRef.current = null
      fitRef.current = null
      if (ref && sid) ref.invoke('Close', sid).catch(() => undefined)
      ref?.stop().catch(() => undefined)
      term.dispose()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [opened, containerGuid, terminalEl])

  return (
    <Modal
      size="xl"
      opened={opened}
      onClose={onClose}
      title={
        <Group gap="sm" align="center">
          <Text fw={700}>{t('admin.content.exec.title')}</Text>
          {containerTitle && (
            <Text size="xs" c="dimmed" ff="monospace">
              {containerTitle}
            </Text>
          )}
          <Text size="xs" c={status === 'connected' ? 'teal' : status === 'error' ? 'red' : 'dimmed'}>
            ({status})
          </Text>
        </Group>
      }
      {...rest}
    >
      <Stack gap="sm">
        <Group justify="space-between" align="center" wrap="nowrap">
          <Group gap="xs" align="center" wrap="nowrap">
            <SegmentedControl
              size="xs"
              data={['sh', 'bash']}
              value={shell}
              onChange={(v) => setShell(v as 'sh' | 'bash')}
              disabled={status === 'connecting' || status === 'connected'}
              aria-label={t('admin.content.exec.shell_label', 'Shell to launch')}
            />
            {(status === 'connecting' || status === 'connected') && (
              <Text size="xs" c="dimmed">
                {t('admin.content.exec.shell_locked', 'Shell is locked while connected — close and reopen to switch')}
              </Text>
            )}
          </Group>
          <Text size="xs" c="dimmed" ff="monospace">
            {secureCtx
              ? t(
                  'admin.content.exec.shortcuts',
                  'Select or Ctrl/⌘+C to copy · Ctrl+Shift+V / Ctrl+V / right-click to paste'
                )
              : t(
                  'admin.content.exec.shortcuts_insecure',
                  'Select to copy · Ctrl+V or right-click to paste · (serve over HTTPS for full clipboard)'
                )}
          </Text>
        </Group>
        {status === 'error' && errorMsg && (
          <Alert color="red" variant="light" title={t('admin.content.exec.error_title', 'Connection error')}>
            <Text size="xs" ff="monospace">
              {errorMsg}
            </Text>
          </Alert>
        )}
        <div
          ref={setTerminalEl}
          role="group"
          aria-label={t('admin.content.exec.terminal_label', 'Container shell terminal')}
          style={{
            height: '50vh',
            background: '#0c0c14',
            padding: 6,
            borderRadius: 4,
          }}
        />
      </Stack>
    </Modal>
  )
}
