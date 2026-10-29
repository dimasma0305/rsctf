// Serialized into the browser; observe visible controls and the page behind portals.
export function installSurfaceProbe() {
  const visible = (e) =>
    e &&
    e.getClientRects().length &&
    getComputedStyle(e).visibility !== "hidden";
  const rect = (e) => {
    if (!visible(e)) return null;
    const { x, y, width, height } = e.getBoundingClientRect();
    return { x, y, width, height };
  };
  window.__surfaceProbe = async (action, duration = 1800) => {
    const frames = [],
      entries = [],
      start = performance.now();
    const selectors = [
      "#main-content",
      "[data-page-content]",
      "#main-content h1",
      "[data-workspace-bar]",
      "[data-event-workspace-header]",
      "[data-event-tabs]",
      "#primary-navigation-rail",
      'header[data-guide-boundary="top-shell"]',
      'nav[aria-label="Mobile navigation"]',
    ];
    const observer = new PerformanceObserver((list) =>
      entries.push(
        ...list.getEntries().map((e) => ({
          startTime: e.startTime,
          value: e.value,
          hadRecentInput: e.hadRecentInput,
          sources: e.sources.map((s) => ({
            node: s.node?.tagName,
            previous: s.previousRect.toJSON(),
            current: s.currentRect.toJSON(),
          })),
        })),
      ),
    );
    observer.observe({ type: "layout-shift" });
    const sample = () => {
      const popup = [
        ...document.querySelectorAll(
          '[role="dialog"], [role="menu"], [role="listbox"], [role="tooltip"]',
        ),
      ].find(visible);
      frames.push({
        time: performance.now() - start,
        path: location.pathname,
        background: selectors.map((s) => rect(document.querySelector(s))),
        popup: rect(popup),
        close: rect(popup?.querySelector('button[aria-label="Close"]')),
        scroll: [
          scrollY,
          document.querySelector("#main-content")?.scrollTop || 0,
        ],
        gutter: innerWidth - document.documentElement.clientWidth,
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
    observer.disconnect();
    return { frames, entries };
  };
}

export function surfaceFailures(
  frames,
  { popup = false, closing = false } = {},
) {
  const failures = [];
  const before = frames[0];
  for (let i = 0; i < before.background.length; i++) {
    const initial = before.background[i];
    if (!initial) continue;
    if (frames.some((f) => !f.background[i]))
      failures.push(`background anchor ${i} disappeared`);
    if (
      frames.some(
        (f) =>
          f.background[i] &&
          ["x", "y", "width"].some(
            (k) => Math.abs(f.background[i][k] - initial[k]) > 1,
          ),
      )
    )
      failures.push(`background anchor ${i} moved`);
  }
  if (frames.some((f) => f.overflow)) failures.push("horizontal overflow");
  if (popup) {
    const settled = frames.filter((f) => f.time >= 300 && f.popup);
    if (!settled.length) failures.push("popup did not open");
    else if (
      settled.some((f) =>
        ["x", "y", "width", "height"].some(
          (k) => Math.abs(f.popup[k] - settled[0].popup[k]) > 1,
        ),
      )
    )
      failures.push("popup resized or moved after entrance");
    if (settled.length && !frames.at(-1).popup)
      failures.push("popup closed before observation ended");
  }
  if (closing && frames.at(-1).popup) failures.push("popup did not close");
  if (
    closing &&
    frames.at(-1).scroll.some((v, i) => Math.abs(v - before.scroll[i]) > 1)
  )
    failures.push("closing changed scroll position");
  return failures;
}
