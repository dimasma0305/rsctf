import { useCallback, useEffect, useState } from 'react'
import { useViewerIdentity } from '@Utils/ViewerIdentity'
import type { ScoreboardItem } from '@Api'

/** Keep the last observed team while a live update moves it off the page. */
export const useScoreboardSelection = (gameId: number, items: ScoreboardItem[] | undefined) => {
  const { scope } = useViewerIdentity()
  const identity = `${gameId}:${scope ?? 'unscoped'}`
  const [selection, setSelection] = useState<{ identity: string; item: ScoreboardItem } | null>(null)
  const selected = selection?.identity === identity ? selection : null
  const currentItem = selected ? (items?.find(item => item.id === selected.item.id) ?? selected.item) : null
  useEffect(() => {
    if (selection && selection.identity !== identity) {
      setSelection(null)
    } else if (currentItem && selected && currentItem !== selected.item) {
      setSelection({ identity, item: currentItem })
    }
  }, [currentItem, selected, selection, identity])
  return {
    currentItem,
    selectItem: useCallback((item: ScoreboardItem) => setSelection({ identity, item }), [identity]),
    clearSelection: useCallback(() => setSelection(null), []),
  }
}
