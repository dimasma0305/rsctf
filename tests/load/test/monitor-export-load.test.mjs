import assert from 'node:assert/strict';
import test from 'node:test';

import {
  assertExportRowBound,
  classifyExportResponse,
  worksheetRowCount,
} from '../monitor-export-model.js';

test('monitor export response classification requires XLSX or retry admission metadata', () => {
  assert.deepEqual(
    classifyExportResponse(
      200,
      'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
      null,
    ),
    { valid: true, admitted: true, overloaded: false },
  );
  assert.deepEqual(classifyExportResponse(429, 'application/json', '3'), {
    valid: true,
    admitted: false,
    overloaded: true,
  });
  assert.equal(classifyExportResponse(503, 'application/json', null).valid, false);
  assert.equal(classifyExportResponse(500, 'application/json', '3').valid, false);
});

test('worksheet integrity counts only row elements and enforces server bounds', () => {
  const xml = '<worksheet><sheetData><row r="1"></row><row r="2"></row></sheetData></worksheet>';
  assert.equal(worksheetRowCount(xml), 2);
  assert.equal(assertExportRowBound('scoreboard', 10_000), 10_000);
  assert.equal(assertExportRowBound('submissions', 50_000), 50_000);
  assert.throws(() => assertExportRowBound('scoreboard', 10_001), /outside the supported/);
});
