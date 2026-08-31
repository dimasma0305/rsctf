import { FileType, FlagCreateModel } from '@Api'

export const MAX_FLAG_IMPORT_ROWS = 100
export const MAX_FLAG_BYTES = 127
export const MAX_FLAG_URL_BYTES = 2048

const byteLength = (value: string) => new TextEncoder().encode(value).byteLength
const FLAG_WHITESPACE_SOURCE =
  '\\u0009-\\u000D\\u0020\\u0085\\u00A0\\u1680\\u2000-\\u200A\\u2028\\u2029\\u202F\\u205F\\u3000\\uFEFF'
const FLAG_WHITESPACE_CHARACTER = new RegExp(`^[${FLAG_WHITESPACE_SOURCE}]$`, 'u')
const REMOTE_FLAG_ROW = new RegExp(
  `^([^${FLAG_WHITESPACE_SOURCE}]+)[${FLAG_WHITESPACE_SOURCE}]+([^${FLAG_WHITESPACE_SOURCE}]+)[${FLAG_WHITESPACE_SOURCE}]*$`,
  'u'
)

export const isFlagWhitespace = (value: string) => FLAG_WHITESPACE_CHARACTER.test(value)
export const isBlankFlagValue = (value: string) => Array.from(value).every(isFlagWhitespace)
export const hasFlagBoundaryWhitespace = (value: string) => {
  const characters = Array.from(value)
  return (
    characters.length > 0 &&
    (isFlagWhitespace(characters[0]) || isFlagWhitespace(characters.at(-1) ?? ''))
  )
}

export const validateFlagRows = (rows: FlagCreateModel[]): string | null => {
  if (rows.length === 0) return 'Enter at least one flag.'
  if (rows.length > MAX_FLAG_IMPORT_ROWS) return `Only ${MAX_FLAG_IMPORT_ROWS} flags can be added at once.`
  for (const row of rows) {
    const bytes = byteLength(row.flag)
    if (bytes === 0 || bytes > MAX_FLAG_BYTES) {
      return `Every flag must contain 1 to ${MAX_FLAG_BYTES} UTF-8 bytes.`
    }
    if (hasFlagBoundaryWhitespace(row.flag)) {
      return 'Flags cannot start or end with whitespace.'
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
    if (isBlankFlagValue(line)) continue
    rows.push({ flag: line })
    if (rows.length > MAX_FLAG_IMPORT_ROWS) break
  }
  return rows
}

export const parseRemoteFlagRows = (text: string): FlagCreateModel[] => {
  const rows: FlagCreateModel[] = []
  for (const line of text.split('\n')) {
    const match = line.match(REMOTE_FLAG_ROW)
    if (!match) continue
    rows.push({ flag: match[1], attachmentType: FileType.Remote, remoteUrl: match[2] })
    if (rows.length > MAX_FLAG_IMPORT_ROWS) break
  }
  return rows
}
