import { FileType, FlagCreateModel } from '@Api'

export const MAX_FLAG_IMPORT_ROWS = 100
export const MAX_FLAG_BYTES = 127
export const MAX_FLAG_URL_BYTES = 2048

const byteLength = (value: string) => new TextEncoder().encode(value).byteLength

export const validateFlagRows = (rows: FlagCreateModel[]): string | null => {
  if (rows.length === 0) return 'Enter at least one flag.'
  if (rows.length > MAX_FLAG_IMPORT_ROWS) return `Only ${MAX_FLAG_IMPORT_ROWS} flags can be added at once.`
  for (const row of rows) {
    const bytes = byteLength(row.flag)
    if (bytes === 0 || bytes > MAX_FLAG_BYTES) {
      return `Every flag must contain 1 to ${MAX_FLAG_BYTES} UTF-8 bytes.`
    }
    if (row.remoteUrl && byteLength(row.remoteUrl) > MAX_FLAG_URL_BYTES) {
      return `Every attachment URL must be at most ${MAX_FLAG_URL_BYTES} UTF-8 bytes.`
    }
  }
  return null
}

export const parsePlainFlagRows = (text: string): FlagCreateModel[] => {
  const rows: FlagCreateModel[] = []
  for (const line of text.split('\n')) {
    if (!line.trim()) continue
    rows.push({ flag: line })
    if (rows.length > MAX_FLAG_IMPORT_ROWS) break
  }
  return rows
}

export const parseRemoteFlagRows = (text: string): FlagCreateModel[] => {
  const rows: FlagCreateModel[] = []
  for (const line of text.split('\n')) {
    const match = line.match(/^(\S+)\s+(\S+)\s*$/)
    if (!match) continue
    rows.push({ flag: match[1], attachmentType: FileType.Remote, remoteUrl: match[2] })
    if (rows.length > MAX_FLAG_IMPORT_ROWS) break
  }
  return rows
}
