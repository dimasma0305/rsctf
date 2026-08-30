import { Button, ButtonProps } from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import { mdiClose, mdiDownload } from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { runDownloadSingleFlight } from '@Utils/DownloadSingleFlight'
import { progressLabel } from '@Utils/SnapshotDownload'

interface SnapshotDownloadButtonProps extends Omit<ButtonProps, 'onClick' | 'loading' | 'leftSection'> {
  url: string
  filename: string
  label: string
  downloadKey: string
}

const saveBlob = (blob: Blob, filename: string) => {
  const url = window.URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.hidden = true
  anchor.href = url
  anchor.download = filename
  document.body.appendChild(anchor)
  anchor.click()
  window.setTimeout(() => {
    anchor.remove()
    window.URL.revokeObjectURL(url)
  })
}

/**
 * Immediate single-flight snapshot download with visible progress and cancel.
 * The backend remains authoritative across tabs/users; this control closes the
 * same-render duplicate-click gap and makes a long transfer understandable.
 */
export const SnapshotDownloadButton: FC<SnapshotDownloadButtonProps> = ({
  url,
  filename,
  label,
  downloadKey,
  ...buttonProps
}) => {
  const { t } = useTranslation()
  const request = useRef<XMLHttpRequest | null>(null)
  const starting = useRef(false)
  const [progress, setProgress] = useState<string | null>(null)

  useEffect(() => () => request.current?.abort(), [])

  const start = () => {
    if (starting.current || request.current) return
    starting.current = true
    setProgress('…')
    void runDownloadSingleFlight(
      downloadKey,
      () =>
        new Promise<void>((resolve, reject) => {
          const xhr = new XMLHttpRequest()
          request.current = xhr
          xhr.open('GET', url)
          xhr.responseType = 'blob'
          xhr.withCredentials = true
          xhr.onprogress = (event) => setProgress(progressLabel(event.loaded, event.total))
          xhr.onerror = () => reject(new Error('Snapshot download failed'))
          xhr.onabort = () => reject(new DOMException('Download cancelled', 'AbortError'))
          xhr.onload = () => {
            if (xhr.status < 200 || xhr.status >= 300) {
              reject(new Error(`Snapshot download failed (${xhr.status})`))
              return
            }
            saveBlob(xhr.response as Blob, filename)
            resolve()
          }
          xhr.send()
        })
    )
      .catch((error: unknown) => {
        if (!(error instanceof DOMException && error.name === 'AbortError')) {
          showNotification({
            color: 'red',
            title: t('common.download.failed', 'Download failed'),
            message: error instanceof Error ? error.message : String(error),
          })
        }
      })
      .finally(() => {
        request.current = null
        starting.current = false
        setProgress(null)
      })
  }

  const cancel = () => request.current?.abort()
  const active = progress !== null
  return (
    <Button
      {...buttonProps}
      leftSection={<Icon path={active ? mdiClose : mdiDownload} size={0.8} aria-hidden="true" />}
      onClick={active ? cancel : start}
      aria-live="polite"
      aria-label={active ? t('common.download.cancel_progress', 'Cancel download ({{progress}})', { progress }) : label}
    >
      {active ? t('common.download.cancel_short', 'Cancel {{progress}}', { progress }) : label}
    </Button>
  )
}
