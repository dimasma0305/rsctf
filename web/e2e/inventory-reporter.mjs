import { mkdirSync, writeFileSync, appendFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { mergedControls, renderInventory } from '../../tests/visual/interaction-report.mjs'
import { sourceMap } from '../../tests/visual/interaction-source-map.mjs'
import { repositoryRoot } from '../../tests/visual/route-catalog.mjs'

export default class InventoryReporter {
  results = []
  expected = 0
  onBegin(_config, suite) {
    this.expected = suite.allTests().filter((t) => t.title.endsWith('maps controls and popup states')).length
  }
  onTestEnd(test, result) {
    if (!test.title.endsWith('maps controls and popup states')) return
    const attachment = result.attachments.find((a) => a.name === 'interaction-inventory')
    this.results.push({
      title: test.title,
      status: result.status,
      inventory: attachment?.body ? JSON.parse(attachment.body.toString()) : null,
    })
  }
  onEnd(result) {
    const source = sourceMap()
    const dir = resolve(repositoryRoot, 'visual-audit-output/interactions')
    mkdirSync(dir, { recursive: true })
    const inventories = this.results.flatMap((r) => (r.inventory ? [r.inventory] : []))
    const controls = inventories.flatMap((r) => r.states.flatMap((s) => s.controls))
    const probes = inventories.flatMap((r) => r.probes)
    const unique = inventories.flatMap(mergedControls)
    const complete = inventories.length === this.expected && inventories.every((r) => r.states.length)
    const summary = {
      status: complete ? result.status : 'failed',
      expectedCases: this.expected,
      completedCases: this.results.length,
      inventoriedCases: inventories.length,
      catalogRoutes: source.routes.length,
      scannedRoutes: new Set(inventories.map((r) => r.route)).size,
      controlObservations: controls.length,
      uniqueControlsPerRouteAndViewport: unique.length,
      controlsByStatus: Object.fromEntries(
        ['discovered', 'exercised', 'hidden', 'disabled'].map((status) => [
          status,
          unique.filter((c) => c.status === status).length,
        ])
      ),
      exercisedOpenCloseProbes: probes.filter((p) => p.status === 'exercised').length,
      unverifiedProbes: probes.filter((p) => p.status !== 'exercised').length,
      sourceCandidates: source.candidates.length,
    }
    writeFileSync(resolve(dir, 'inventory.json'), JSON.stringify({ summary, source, results: this.results }, null, 2))
    writeFileSync(resolve(dir, 'inventory.html'), renderInventory(summary, inventories, source))
    const escape = (s) => String(s).replaceAll('|', '\\|').replaceAll('\n', ' ')
    const lines = [
      '# Frontend interaction inventory',
      '',
      'Discovery is not behavior coverage. Counts are observations in fixture states, not all possible UI states.',
      '',
      ...Object.entries(summary).map(
        ([key, value]) => `- ${key}: ${typeof value === 'object' ? JSON.stringify(value) : value}`
      ),
      '',
      '| View | Route | Unique controls | Open/close verified | Unverified probes |',
      '| --- | --- | ---: | ---: | ---: |',
      ...inventories.map(
        (r) =>
          `| ${r.project} | ${escape(r.route)} | ${mergedControls(r).length} | ${r.probes.filter((p) => p.status === 'exercised').length} | ${r.probes.filter((p) => p.status !== 'exercised').length} |`
      ),
      '',
      'See inventory.json for every control, accessible name, selector, state, disabled/hidden status, untested reason, source candidates and route-to-source imports.',
      'Playwright HTML report contains per-route attachments, failures, screenshots and traces. Existing full-layout/navigation suites remain required.',
      '',
    ]
    writeFileSync(resolve(dir, 'inventory.md'), lines.join('\n'))
    if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, lines.join('\n'))
    // An interrupted/skipped route must not look like a complete successful scan.
    if (!complete) return { status: 'failed' }
  }
}
