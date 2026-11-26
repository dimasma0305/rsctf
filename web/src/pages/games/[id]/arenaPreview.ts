import { mdiChessKing, mdiFlagOutline, mdiSwordCross } from '@mdi/js'

export const milestoneKinds = {
  ad: { mode: 'Attack & Defense', title: 'First blood', icon: mdiSwordCross, accent: 'var(--red)' },
  jeopardy: { mode: 'Jeopardy', title: 'First blood', icon: mdiFlagOutline, accent: 'var(--amber)' },
  koth: { mode: 'King of the Hill', title: 'First crown', icon: mdiChessKing, accent: 'var(--violet)' },
}

export type MilestoneKind = keyof typeof milestoneKinds

export function milestoneIcon(kind: MilestoneKind) {
  return `<svg aria-hidden="true" viewBox="0 0 24 24"><path fill="currentColor" d="${milestoneKinds[kind].icon}"/></svg>`
}

function alertButton(id: string, kind: MilestoneKind) {
  const event = milestoneKinds[kind]
  return `<button type="button" class="event-choice" id="${id}" style="--event-color:${event.accent}">
    <span class="event-icon">${milestoneIcon(kind)}</span>
    <span><small>${event.mode}</small><strong>${event.title}</strong></span>
    <span class="event-play" aria-hidden="true">▸</span></button>`
}

export const previewPanel = `
  <section class="panel preview-panel" id="previewPanel" hidden aria-labelledby="previewTitle">
    <header><div><h2 id="previewTitle">Arena preview</h2><p>Play an alert with simulated teams and scores.</p></div></header>
    <div class="event-choices">
      ${alertButton('fbAdBtn', 'ad')}
      ${alertButton('fbJeoBtn', 'jeopardy')}
      ${alertButton('fbKothBtn', 'koth')}
    </div>
    <div class="preview-match" role="group" aria-label="Match events">
      <button type="button" class="btn" id="impactBtn">Simulate impact</button>
      <button type="button" class="btn" id="patchBtn">Service patch</button>
      <button type="button" class="btn" id="freezeBtn" aria-pressed="false">Freeze standings</button>
      <button type="button" class="btn" id="endBtn">End preview match</button>
    </div>
    <details class="preview-setup"><summary>Simulation settings</summary>
      <div class="scenario-inputs">
        <label class="cfg">Teams<input id="cfgTeams" type="number" min="2" max="20" value="8"></label>
        <label class="cfg">A&amp;D services<input id="cfgAd" type="number" min="0" max="10" value="4"></label>
        <label class="cfg">KotH hills<input id="cfgKoth" type="number" min="0" max="12" value="3"></label>
        <label class="cfg">Jeopardy challenges<input id="cfgJeop" type="number" min="0" max="40" value="24"></label>
      </div>
    </details>
  </section>`
