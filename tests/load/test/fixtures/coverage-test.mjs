#!/usr/bin/env node
// Unit-test protocol fixture only. Its profile bytes are intentionally NOT
// LLVM coverage, and may never be used as the real instrumentation proof.
import { writeFileSync } from 'node:fs'
import { basename } from 'node:path'

const target = basename(process.argv[1])
const all = target === 'library' ? ['alpha', 'bravo', 'charlie']
  : target === 'integration' ? ['alpha', 'delta'] : []
const args = process.argv.slice(2)
const profile = process.env.LLVM_PROFILE_FILE.replaceAll('%p', String(process.pid)).replaceAll('%m', 'fixture')
const listing = args.includes('--list')
const selectedShard = profile.includes('db-shard-1-')
const fault = process.env.RSCTF_COVERAGE_FIXTURE_FAULT
if (!(fault === 'missing-profile' && selectedShard)) writeFileSync(profile, 'unit fixture, not LLVM coverage\n')
if (listing) {
  for (const name of all) console.log(`${name}: test`)
  console.log(`\n${all.length} tests, 0 benchmarks`)
} else {
  if (fault === 'exit-failure' && selectedShard) process.exit(5)
  const selected = args.slice(args.indexOf('--exact') + 1)
  if (!selected.every((name) => all.includes(name))) process.exit(6)
  const executed = fault === 'missing-test' && selectedShard ? selected.slice(1) : selected
  for (const name of executed) console.log(`test ${name} ... ok`)
  console.log(`test result: ok. ${executed.length} passed; 0 failed; 0 ignored; 0 filtered out; finished in 0.01s`)
}
