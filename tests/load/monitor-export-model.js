export const SCOREBOARD_EXPORT_ROW_LIMIT = 10_000;
export const SUBMISSION_EXPORT_ROW_LIMIT = 50_000;

export function worksheetRowCount(xml) {
  return (String(xml).match(/<row(?:\s|>)/g) || []).length;
}

export function classifyExportResponse(status, contentType, retryAfter) {
  if (status === 200) {
    return {
      valid: String(contentType || '').toLowerCase().includes('spreadsheetml.sheet'),
      admitted: true,
      overloaded: false,
    };
  }
  if (status === 429 || status === 503) {
    const seconds = Number(retryAfter);
    return {
      valid: Number.isSafeInteger(seconds) && seconds > 0,
      admitted: false,
      overloaded: true,
    };
  }
  return { valid: false, admitted: false, overloaded: false };
}

export function assertExportRowBound(kind, rows) {
  const limit = kind === 'scoreboard' ? SCOREBOARD_EXPORT_ROW_LIMIT : SUBMISSION_EXPORT_ROW_LIMIT;
  if (!Number.isSafeInteger(rows) || rows < 0 || rows > limit) {
    throw new Error(`${kind} export row count ${rows} is outside the supported 0..${limit} bound`);
  }
  return rows;
}
