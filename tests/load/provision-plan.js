const HOUR_MS = 60 * 60 * 1_000;
const STAGING_LEAD_MS = 24 * HOUR_MS;

/**
 * Keep organizer setup outside the immutable scoring window, then arm the
 * finished fixture with the historical live schedule used by the lifecycle.
 */
export function stagedEventSchedule(nowMs, eventDurationMs) {
  if (
    !Number.isSafeInteger(nowMs) ||
    !Number.isSafeInteger(eventDurationMs) ||
    eventDurationMs <= 0
  ) {
    throw new Error('lifecycle schedule requires safe positive integer milliseconds');
  }
  const schedule = {
    stagingStart: nowMs + STAGING_LEAD_MS,
    stagingEnd: nowMs + STAGING_LEAD_MS + eventDurationMs,
    liveStart: nowMs - HOUR_MS,
    liveEnd: nowMs + eventDurationMs,
  };
  if (Object.values(schedule).some((value) => !Number.isSafeInteger(value))) {
    throw new Error('lifecycle schedule exceeds the safe timestamp range');
  }
  return Object.freeze(schedule);
}
