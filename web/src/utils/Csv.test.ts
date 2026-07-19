import assert from 'node:assert/strict'
import test from 'node:test'
import { quoteSpreadsheetCsvCell } from './Csv'

test('spreadsheet CSV cells neutralize formula prefixes', () => {
  for (const value of [
    '=HYPERLINK("https://example.test")',
    '+1+1',
    '-2+3',
    '@SUM(1,2)',
    '  =CMD()',
    '\t=CMD()',
    '\r=CMD()',
  ]) {
    const encoded = quoteSpreadsheetCsvCell(value)
    assert.ok(encoded.startsWith('"\''), `${value} was not neutralized: ${encoded}`)
  }
})

test('spreadsheet CSV cells preserve ordinary values and quote delimiters', () => {
  assert.equal(quoteSpreadsheetCsvCell('alice@example.test'), '"alice@example.test"')
  assert.equal(quoteSpreadsheetCsvCell('Doe, "Alice"'), '"Doe, ""Alice"""')
  assert.equal(quoteSpreadsheetCsvCell(undefined), '""')
})
