/**
 * Exercise report generation concurrently, then read once more after every
 * mutation-capable sweep has completed. Only the final response is an
 * authoritative snapshot; concurrent response bodies are race observations.
 */
export async function loadAuthoritativeAfterConcurrentSweep(
  fetchReport,
  concurrency = 3,
) {
  if (typeof fetchReport !== "function") {
    throw new TypeError("fetchReport must be a function");
  }
  if (!Number.isSafeInteger(concurrency) || concurrency < 1) {
    throw new RangeError("concurrency must be an integer >= 1");
  }

  const sweep = await Promise.all(
    Array.from({ length: concurrency }, (_, index) => fetchReport(index)),
  );
  const authoritative = await fetchReport(concurrency);
  return { sweep, authoritative };
}
