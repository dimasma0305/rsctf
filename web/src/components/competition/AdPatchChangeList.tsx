import { Button, Text } from '@mantine/core'
import { useState } from 'react'
import { useTranslation } from 'react-i18next'
import type { AdPatchPreview } from '@Api'

export function AdPatchChangeList({ changes }: { changes: AdPatchPreview['changes'] }) {
  const { t } = useTranslation()
  const [limit, setLimit] = useState(50)
  if (!changes.length) return null
  return <>
    <ul style={{ overflowWrap: 'anywhere', margin: 0, paddingInlineStart: '1.25rem' }}>
      {changes.slice(0, limit).map(change => (
        <li key={change.path}><Text size="sm" span>{t(`game.ad.patch.kind.${change.kind}`, change.kind)} · <code>{change.path}</code></Text></li>
      ))}
    </ul>
    {changes.length > limit && <Button variant="subtle" size="xs" onClick={() => setLimit(limit + 50)}>{t('game.ad.patch.more', 'Show more files')}</Button>}
  </>
}
