// CLS session windows: less than 1s between entries and less than 5s total.
export function sessionScore(entries, includeRecentInput = false) {
  let maximum = 0,
    sum = 0,
    first = -Infinity,
    previous = -Infinity;
  for (const entry of entries) {
    if (!includeRecentInput && entry.hadRecentInput) continue;
    if (entry.startTime - previous < 1000 && entry.startTime - first < 5000)
      sum += entry.value;
    else {
      first = entry.startTime;
      sum = entry.value;
    }
    previous = entry.startTime;
    maximum = Math.max(maximum, sum);
  }
  return maximum;
}

/** Runs in Chromium. Keep bounded evidence, including shifts masked by recent input. */
export function installLayoutProbe() {
  const rect = (element) => {
    if (!element || !element.getClientRects().length) return null;
    const r = element.getBoundingClientRect();
    return { x: r.x, y: r.y, width: r.width, height: r.height };
  };
  const label = (node) =>
    node instanceof Element
      ? node.tagName.toLowerCase() +
        (node.id ? `#${node.id}` : "") +
        [...node.attributes]
          .filter((a) => a.name.startsWith("data-"))
          .map((a) => `[${a.name}]`)
          .join("")
      : null;
  const entries = [];
  const observer = new PerformanceObserver((list) => {
    for (const entry of list.getEntries()) {
      if (entries.length >= 2000)
        throw new Error("Layout probe entry budget exceeded");
      entries.push({
        startTime: entry.startTime,
        value: entry.value,
        hadRecentInput: entry.hadRecentInput,
        sources: entry.sources.map((source) => ({
          node: label(source.node),
          previous: source.previousRect.toJSON(),
          current: source.currentRect.toJSON(),
        })),
      });
    }
  });
  observer.observe({ type: "layout-shift", buffered: true });
  window.__layoutProbe = {
    entries,
    async measure(action, duration = 4000) {
      const start = performance.now(),
        sourceMain = document.getElementById("main-content");
      const frames = [];
      const sample = () => {
        const main = document.getElementById("main-content");
        frames.push({
          time: performance.now() - start,
          path: location.pathname,
          destination: main !== sourceMain,
          rail: rect(document.querySelector("#primary-navigation-rail")),
          mobileHeader: rect(
            document.querySelector('header[data-guide-boundary="top-shell"]'),
          ),
          dock: rect(
            document.querySelector('nav[aria-label="Mobile navigation"]'),
          ),
          main: rect(main),
          title: rect(main?.querySelector("h1")),
          workspace: rect(document.querySelector("[data-workspace-bar]")),
          eventHeader: rect(
            document.querySelector("[data-event-workspace-header]"),
          ),
          eventTabs: rect(document.querySelector("[data-event-tabs]")),
          toolbar: rect(document.querySelector("[data-challenge-toolbar]")),
          form: rect(main?.querySelector("[data-page-content] form")),
          input: rect(
            main?.querySelector(
              '[data-page-content] input:not([type="hidden"])',
            ),
          ),
          team: rect(document.querySelector("[data-team-summary]")),
          scroll: { window: scrollY, main: main?.scrollTop ?? 0 },
          overflow: document.documentElement.scrollWidth > innerWidth + 1,
        });
      };
      sample();
      action();
      await new Promise((resolve) => {
        const tick = () => {
          sample();
          if (performance.now() - start < duration) requestAnimationFrame(tick);
          else resolve();
        };
        requestAnimationFrame(tick);
      });
      return {
        frames,
        entries: entries.filter((entry) => entry.startTime >= start),
      };
    },
  };
}

export function geometryFailures(
  frames,
  { event = false, tolerance = 1 } = {},
) {
  const failures = [];
  for (const key of [
    "rail",
    "mobileHeader",
    "dock",
    ...(event ? ["eventHeader", "eventTabs"] : []),
  ]) {
    const baseline = frames[0]?.[key];
    if (!baseline) continue;
    for (const frame of frames) {
      const current = frame[key];
      if (!current) {
        failures.push(`${key} disappeared at ${Math.round(frame.time)}ms`);
        break;
      }
      if (
        ["x", "y", "width", "height"].some(
          (axis) => Math.abs(current[axis] - baseline[axis]) > tolerance,
        )
      ) {
        failures.push(`${key} moved at ${Math.round(frame.time)}ms`);
        break;
      }
    }
  }
  // A new page can have a different layout, but its already-painted controls
  // must not move again when its delayed reads resolve.
  const destination = frames.filter((frame) => frame.destination && frame.main);
  for (const key of [
    "title",
    "workspace",
    "eventTabs",
    "toolbar",
    "form",
    "input",
    "team",
  ]) {
    const baseline = destination.find((frame) => frame[key])?.[key];
    if (!baseline) continue;
    const axes = key === "team" ? ["y", "height"] : ["x", "y"];
    if (
      destination.some(
        (frame) =>
          frame[key] &&
          axes.some(
            (axis) => Math.abs(frame[key][axis] - baseline[axis]) > tolerance,
          ),
      )
    )
      failures.push(`destination ${key} moved during loading`);
  }
  if (frames.some((frame) => frame.overflow))
    failures.push("horizontal page overflow");
  return failures;
}
