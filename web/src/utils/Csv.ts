/**
 * Quote an untrusted CSV cell for spreadsheet consumption. CSV quoting alone
 * does not stop Excel/LibreOffice from evaluating a leading formula marker, so
 * prefix dangerous cells with an apostrophe before escaping quotes.
 */
export function quoteSpreadsheetCsvCell(value: string | null | undefined): string {
  const raw = value ?? ''
  const safe = /^[\t\r\n ]*[=+\-@]/.test(raw) || /^[\t\r\n]/.test(raw) ? `'${raw}` : raw
  return `"${safe.replace(/"/g, '""')}"`
}
