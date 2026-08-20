export const EVENT_TELEMETRY_LOGICAL_LIMIT = 256 * 1024 * 1024;

export function boundedInteger(value, label, minimum, maximum) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${label} must be an integer from ${minimum} to ${maximum}`);
  }
  return parsed;
}

export function parsePeerFixture(value) {
  const [userId, participationId, peerId, bucketMs] = String(value || "").split("|");
  const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
  const participation = Number(participationId);
  const bucket = Number(bucketMs);
  if (!uuid.test(userId) || !uuid.test(peerId) || !Number.isSafeInteger(participation) || participation <= 0) {
    throw new Error("the selected event needs one live event-VPN peer");
  }
  if (!Number.isSafeInteger(bucket) || bucket <= 0 || bucket % 300_000 !== 0) {
    throw new Error("the event has no valid five-minute telemetry bucket");
  }
  return { userId, participationId: participation, peerId, bucketMs: bucket };
}

export function parseUsage(value) {
  const [logicalBytes, rowCount, disabled, physicalBytes] = String(value || "").split("|");
  const parsed = {
    logicalBytes: Number(logicalBytes || 0),
    rowCount: Number(rowCount || 0),
    disabled: disabled === "t",
    physicalBytes: Number(physicalBytes || 0),
  };
  if (
    !Number.isSafeInteger(parsed.logicalBytes) || parsed.logicalBytes < 0 ||
    !Number.isSafeInteger(parsed.rowCount) || parsed.rowCount < 0 ||
    !Number.isSafeInteger(parsed.physicalBytes) || parsed.physicalBytes < 0
  ) {
    throw new Error(`invalid event telemetry usage row: ${value}`);
  }
  return parsed;
}

export function summarizeResourceSamples(samples) {
  const grouped = new Map();
  for (const sample of samples) {
    for (const container of sample.containers || []) {
      const current = grouped.get(container.name) || { cpu: [], memory: [] };
      current.cpu.push(container.cpuPercent);
      current.memory.push(container.memoryBytes);
      grouped.set(container.name, current);
    }
  }
  return [...grouped.entries()].map(([name, values]) => ({
    name,
    samples: values.cpu.length,
    averageCpuPercent: values.cpu.reduce((sum, value) => sum + value, 0) / values.cpu.length,
    maxCpuPercent: Math.max(...values.cpu),
    maxMemoryBytes: Math.max(...values.memory),
  }));
}

export function parseProcessStat(
  value,
  clockTicks,
  pageSize,
  previous = null,
  observedAtMs = Date.now(),
  name = "rsctf-process",
) {
  if (!Number.isSafeInteger(clockTicks) || clockTicks <= 0 || !Number.isSafeInteger(pageSize) || pageSize <= 0) {
    throw new Error("invalid process clock/page configuration");
  }
  const stat = String(value || "");
  const commandEnd = stat.lastIndexOf(") ");
  const fields = commandEnd >= 0 ? stat.slice(commandEnd + 2).trim().split(/\s+/) : [];
  // The slice begins at proc field 3. utime/stime are 14/15 and RSS is 24.
  const userTicks = Number(fields[11]);
  const systemTicks = Number(fields[12]);
  const residentPages = Number(fields[21]);
  if (
    fields.length < 22 || !Number.isSafeInteger(userTicks) || userTicks < 0 ||
    !Number.isSafeInteger(systemTicks) || systemTicks < 0 ||
    !Number.isSafeInteger(residentPages) || residentPages < 0 ||
    !Number.isFinite(observedAtMs)
  ) {
    throw new Error(`invalid process stat sample: ${value}`);
  }
  const state = { totalTicks: userTicks + systemTicks, observedAtMs };
  if (!previous) return { sample: null, state };
  const elapsedMs = observedAtMs - previous.observedAtMs;
  const elapsedTicks = state.totalTicks - previous.totalTicks;
  if (elapsedMs <= 0 || elapsedTicks < 0) throw new Error("non-monotonic process stat sample");
  return {
    sample: {
      name,
      cpuPercent: (elapsedTicks * 100_000) / (clockTicks * elapsedMs),
      memoryBytes: residentPages * pageSize,
    },
    state,
  };
}

export function k6PhaseSummary(summary) {
  // k6 1.x nested aggregate fields under `values`; k6 2.x writes them
  // directly on each metric and calls a Rate aggregate `value`. Accept both
  // so retained benchmark JSON cannot silently zero a successful live run.
  const values = (name) => {
    const metric = summary?.metrics?.[name] || {};
    return metric.values || metric;
  };
  const rate = (name) => {
    const metric = values(name);
    if (Number.isFinite(metric.rate)) return metric.rate;
    if (Number.isFinite(metric.value)) return metric.value;
    const samples = Number(metric.passes || 0) + Number(metric.fails || 0);
    return samples > 0 ? Number(metric.passes || 0) / samples : 0;
  };
  return {
    requests: values("http_reqs").count || 0,
    requestRate: values("http_reqs").rate || 0,
    p50Ms: values("event_security_ingest_ms").med || 0,
    p95Ms: values("event_security_ingest_ms")["p(95)"] || 0,
    p99Ms: values("event_security_ingest_ms")["p(99)"] || 0,
    maxMs: values("event_security_ingest_ms").max || 0,
    server5xxRate: rate("server_5xx"),
    invalidResponseRate: rate("invalid_response"),
    quotaDropRate: rate("quota_dropped"),
    droppedIterations: values("dropped_iterations").count || 0,
  };
}
