import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'

test('navigation rejects stale links and unmapped domains/pages without depending on cwd', context => {
  const root = mkdtempSync(join(tmpdir(), 'rsctf navigation '))
  context.after(() => rmSync(root, { recursive: true, force: true }))
  const skill = join(root, '.agents/skills/rsctf-engineering')
  mkdirSync(join(skill, 'scripts'), { recursive: true })
  mkdirSync(join(skill, 'references'))
  mkdirSync(join(root, 'web/src/pages'), { recursive: true })
  const script = join(skill, 'scripts/check-navigation.mjs')
  copyFileSync(new URL('./check-navigation.mjs', import.meta.url), script)
  writeFileSync(join(root, 'AGENTS.md'), '[skill](.agents/skills/rsctf-engineering/SKILL.md)\n')
  writeFileSync(join(skill, 'SKILL.md'), '[map](references/test-map.md)\n')
  const page = join(root, 'web/src/pages/Index.tsx')
  writeFileSync(page, 'export default function Home() {}\n')
  for (const group of ['controllers', 'services', 'hubs', 'storage']) {
    mkdirSync(join(root, `src/${group}`), { recursive: true })
    writeFileSync(join(root, `src/${group}/mod.rs`), 'pub mod owner;\n')
    writeFileSync(join(root, `src/${group}/owner.rs`), '')
  }
  writeFileSync(join(skill, 'references/target with spaces.md'), '# Target\n')
  const map = join(skill, 'references/test-map.md')
  const good = ['controllers', 'services', 'hubs', 'storage']
    .map(group => `[owner](../../../../src/${group}/owner.rs)`)
    .concat('[page](../../../../web/src/pages/Index.tsx)',
      '[nested [id]](target%20with%20spaces.md)', '[external](https://example.invalid/missing)',
      '```sh\n[example](missing.md)\n```').join('\n')
  const run = () => spawnSync(process.execPath, [script], { cwd: tmpdir(), encoding: 'utf8' })
  writeFileSync(map, good)
  assert.equal(run().status, 0)
  for (const [link, message] of [
    ['[missing](missing.md)', /missing target/],
    ['[outside](../../../../../outside.md)', /stay inside/],
    ['[absolute](/etc/passwd)', /stay inside/],
    ['[invalid](%ZZ.md)', /invalid destination/],
  ]) {
    writeFileSync(map, `${good}\n${link}\n`)
    const result = run()
    assert.equal(result.status, 1, result.stderr)
    assert.match(result.stderr, message)
  }
  writeFileSync(map, good)
  writeFileSync(join(root, 'src/services/mod.rs'), 'pub mod owner;\npub(crate) mod new_domain;\n')
  assert.match(run().stderr, /Unmapped services module: src\/services\/new_domain/)
  writeFileSync(join(root, 'src/services/mod.rs'), 'pub mod owner;\n')
  writeFileSync(join(root, 'web/src/pages/New.tsx'), '')
  assert.match(run().stderr, /Unmapped page: web\/src\/pages\/New.tsx/)
  writeFileSync(map, good.replace('[page](../../../../web/src/pages/Index.tsx)',
    '[broad root](../../../../web/src/pages/)'))
  assert.match(run().stderr, /Unmapped page/)
})
