import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import test from 'node:test'

const driver = resolve(import.meta.dirname, '../../../scripts/benchmarks/linker-driver.sh')
test('linker experiment preserves the original output and replays identical inputs with both linkers', () => {
  const sysroot = spawnSync('rustc', ['--print', 'sysroot'], { encoding: 'utf8' })
  assert.equal(sysroot.status, 0)
  const host = spawnSync('rustc', ['-Vv'], { encoding: 'utf8' }).stdout.match(/^host: (.+)$/m)[1]
  const directory = mkdtempSync(join(tmpdir(), 'rsctf-linker-known-'))
  try {
    const results = join(directory, 'results'); mkdirSync(results)
    const linker = join(directory, 'lld'); mkdirSync(linker)
    symlinkSync(`${sysroot.stdout.trim()}/lib/rustlib/${host}/bin/rust-lld`, join(linker, 'ld.lld'))
    const source = join(directory, 'known.c')
    writeFileSync(source, '#include <stdio.h>\nint main(void) { puts("linker-known-result"); return 0; }\n')
    const object = join(directory, 'known.o')
    assert.equal(spawnSync('cc', ['-c', source, '-o', object]).status, 0)
    const output = join(directory, 'rsctf-known')
    const args = [object, '-Wl,-z,relro,-z,now', '-o', output]
    const env = { ...process.env, RSCTF_LINK_BENCH_DIR: results, RSCTF_LINK_BENCH_LLD_DIR: linker }
    const linked = spawnSync('bash', [driver, ...args], { env, encoding: 'utf8', timeout: 30000 })
    assert.equal(linked.status, 0, linked.stderr)
    const path = join(results, 'server-links')
    assert.deepEqual(readFileSync(join(path, 'arguments.nul'), 'utf8').split('\0').slice(0, -1), args)
    const binaries = readdirSync(path).filter(name => name.endsWith('.bin'))
    assert.equal(binaries.length, 8)
    for (const binary of [output, ...binaries.map(name => join(path, name))]) {
      const result = spawnSync(binary, [], { encoding: 'utf8' })
      assert.equal(result.status, 0); assert.equal(result.stdout, 'linker-known-result\n')
      const elf = spawnSync('readelf', ['-l', '-d', binary], { encoding: 'utf8' })
      assert.match(elf.stdout, /GNU_RELRO/); assert.match(elf.stdout, /BIND_NOW/)
    }
    for (const file of readdirSync(path).filter(name => name.endsWith('.seconds.log'))) {
      assert.match(readFileSync(join(path, file), 'utf8').trim(), /^\d+\.\d+$/)
    }
    assert.notEqual(spawnSync('bash', [driver, ...args], { env, encoding: 'utf8' }).status, 0, 'must not overwrite evidence')
    const failed = join(directory, 'failed'); mkdirSync(failed)
    assert.notEqual(spawnSync('bash', [driver, `${directory}/missing.o`, '-o', output], {
      env: { ...env, RSCTF_LINK_BENCH_DIR: failed }, encoding: 'utf8',
    }).status, 0, 'link failures must propagate')
  } finally { rmSync(directory, { recursive: true, force: true }) }
})
