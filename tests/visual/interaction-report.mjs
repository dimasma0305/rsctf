export const escapeHtml = (value) =>
  String(value ?? "").replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ],
  );

export function mergedControls(inventory) {
  const merged = new Map();
  for (const state of inventory.states)
    for (const control of state.controls) {
      const key = JSON.stringify([
        control.selector,
        control.role,
        control.name,
        control.href,
      ]);
      const prior = merged.get(key);
      if (!prior) merged.set(key, { ...control, states: [state.name] });
      else {
        prior.states.push(state.name);
        if (
          control.status === "exercised" ||
          (prior.status === "hidden" && !control.hidden)
        )
          Object.assign(prior, control);
      }
    }
  return [...merged.values()];
}

export function renderInventory(summary, inventories, source) {
  const esc = escapeHtml;
  const rows = inventories
    .flatMap((r) =>
      mergedControls(r).map(
        (c) => `<tr>
    <td>${esc(r.project)}</td><td>${esc(r.route)}</td><td>${esc(c.role || c.tag)}</td>
    <td>${esc(c.name || "(no accessible name)")}</td><td>${esc(c.status)}</td>
    <td>${esc(c.reason)}<br>${esc(c.href)}</td><td><details><summary>Locator and states</summary>
    <code>${esc(c.selector)}</code><p>${esc(c.states.join("\n"))}</p></details></td></tr>`,
      ),
    )
    .join("\n");
  return `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
    <title>RSCTF interaction inventory</title><style>
    body{font:15px system-ui;margin:24px;color:#e8edf5;background:#151b26}a{color:#9acaff}
    input{font:inherit;padding:10px;width:min(90%,650px)}table{border-collapse:collapse;width:100%;margin-top:20px}
    th,td{padding:8px;border:1px solid #536078;text-align:left;vertical-align:top}code{overflow-wrap:anywhere}
    tr[hidden]{display:none}summary{cursor:pointer}td p{white-space:pre-line}dt{font-weight:bold}dd{margin-bottom:8px}
    </style><h1>Frontend interaction inventory</h1>
    <p>Discovery is not behavior coverage. “Exercised” means the popup/disclosure open-and-close assertions passed.
    Other actions still need explicit scenarios. This scan uses one fixture identity per route.</p>
    <p><a href="inventory.json">Full JSON and source map</a> · <a href="playwright/index.html">Playwright report</a></p>
    <details><summary>Run summary and source owners</summary><dl>${Object.entries(
      summary,
    )
      .map(
        ([k, v]) =>
          `<dt>${esc(k)}</dt><dd>${esc(typeof v === "object" ? JSON.stringify(v) : v)}</dd>`,
      )
      .join("")}</dl>
    ${source.routes.map((r) => `<details><summary>${esc(r.path)} — ${esc(r.sourceFile)}</summary><ul>${r.sources.map((s) => `<li>${esc(s)}</li>`).join("")}</ul></details>`).join("")}</details>
    <p><label for="filter">Filter by route, name, role or coverage status</label></p><input id="filter" type="search">
    <p id="count" role="status"></p><div style="overflow:auto"><table><thead><tr><th>Viewport</th><th>Route</th><th>Role/tag</th><th>Name</th><th>Status</th><th>Reason / destination</th><th>Details</th></tr></thead><tbody>${rows}</tbody></table></div>
    <script>const rows=[...document.querySelectorAll('tbody tr')];const filter=document.querySelector('#filter');function update(){const q=filter.value.toLowerCase();let count=0;for(const row of rows){row.hidden=!row.textContent.toLowerCase().includes(q);if(!row.hidden)count++}document.querySelector('#count').textContent=count+' of '+rows.length+' controls';}filter.addEventListener('input',update);update();</script></html>`;
}
