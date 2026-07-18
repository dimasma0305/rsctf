import type JSZip from 'jszip'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import test from 'node:test'
import { buildAttackDefenseTemplate } from './SubmitTemplates'

async function textFile(zip: JSZip, path: string): Promise<string> {
  const entry = zip.file(path)
  assert.ok(entry, `generated ZIP is missing ${path}`)
  return entry.async('string')
}

const SHUFFLE_HARNESS = String.raw`
import os
import sys
import types
from unittest.mock import patch

module = types.ModuleType("generated_checker_lib")
module.__file__ = "checker/lib.py"
sys.modules[module.__name__] = module
exec(compile(sys.stdin.read(), module.__file__, "exec"), module.__dict__)

calls = []

@module.checker
def first(_context):
    calls.append("first")

@module.checker
def second(_context):
    calls.append("second")

environment = {
    "RSCTF_ACTION": "check",
    "RSCTF_TARGET_IP": "127.0.0.1",
    "RSCTF_TARGET_PORT": "8080",
    "RSCTF_ROUND": "1",
    "RSCTF_TEAM_ID": "1",
    "RSCTF_CHALLENGE_ID": "1",
    "RSCTF_FLAG": "rsctf{shuffle_test}",
}

for random_index, expected in [(1, ["first", "second"]), (0, ["second", "first"])]:
    calls.clear()
    bounds = []

    def randbelow(bound):
        bounds.append(bound)
        return random_index

    with patch.dict(os.environ, environment, clear=True):
        with patch.object(module.secrets, "randbelow", side_effect=randbelow):
            verdict = module.run_ad_checker()
    if verdict != 0 or calls != expected or bounds != [2]:
        raise RuntimeError((random_index, verdict, calls, bounds))

module._registered_checkers.clear()
calls.clear()

@module.checker
def reports_mumble(_context):
    calls.append("mumble")
    raise module.Mumble("bad response")

@module.checker
def still_runs(_context):
    calls.append("after failure")

with patch.dict(os.environ, environment, clear=True):
    with patch.object(module.secrets, "randbelow", return_value=1):
        verdict = module.run_ad_checker()
if verdict != 1 or calls != ["mumble", "after failure"]:
    raise RuntimeError(("failure continuation", verdict, calls))

print("ok")
`

test('generated A&D checker pins httpx and keeps its library protocol-neutral', async () => {
  const JSZipModule = await import('jszip')
  const blob = await buildAttackDefenseTemplate()
  const zip = await JSZipModule.default.loadAsync(await blob.arrayBuffer())

  assert.equal(await textFile(zip, 'checker/requirements.txt'), 'httpx==0.28.1\n')

  const library = await textFile(zip, 'checker/lib.py')
  assert.match(library, /^"""Protocol-neutral, dependency-free helpers/)
  assert.match(library, /import secrets/)
  assert.match(library, /def checker\(/)
  assert.match(library, /def _shuffled_checkers\(\)/)
  assert.match(library, /secrets\.randbelow\(index \+ 1\)/)
  assert.match(library, /raise max\(failures, key=_failure_priority\)/)
  assert.doesNotMatch(library, /secrets\.choice/)
  assert.match(library, /def run_ad_checker\(\)/)
  assert.match(library, /def run_koth_checker\(\)/)
  assert.match(library, /def ad_checker\(/)
  assert.match(library, /def koth_checker\(/)
  assert.doesNotMatch(library, /httpx|HTTPConnection|socket/)
})

test('generated A&D checker uses focused checks with bounded HTTP', async () => {
  const JSZipModule = await import('jszip')
  const blob = await buildAttackDefenseTemplate()
  const zip = await JSZipModule.default.loadAsync(await blob.arrayBuffer())
  const checker = await textFile(zip, 'checker/run.py')

  assert.match(checker, /import httpx/)
  assert.match(checker, /follow_redirects=False/)
  assert.match(checker, /trust_env=False/)
  assert.match(checker, /client\.stream\(/)
  assert.match(checker, /headers=\{"Accept-Encoding": "identity", "Connection": "close"\}/)
  assert.match(checker, /response\.iter_raw\(chunk_size=1024\)/)
  assert.match(checker, /len\(body\) \+ len\(chunk\) > MAX_RESPONSE_BYTES/)
  assert.match(checker, /timeout=REQUEST_TIMEOUT_SECONDS/)
  assert.doesNotMatch(checker, /HTTPConnection/)

  const checks = checker.split('\n@checker\n').slice(1)
  assert.equal(checks.length, 2)
  assert.match(checks[0], /def check_health\(/)
  assert.match(checks[0], /http_get\(context, "\/health"\) != "ok"/)
  assert.doesNotMatch(checks[0], /context\.flag/)
  assert.match(checks[1], /def check_flag\(/)
  assert.match(checks[1], /http_get\(context, "\/flag"\) != context\.flag/)
  assert.match(checker, /raise SystemExit\(run_ad_checker\(\)\)/)
})

test('generated checker library runs every function in both shuffled orders', async () => {
  const JSZipModule = await import('jszip')
  const blob = await buildAttackDefenseTemplate()
  const zip = await JSZipModule.default.loadAsync(await blob.arrayBuffer())
  const library = await textFile(zip, 'checker/lib.py')

  const output = execFileSync('python3', ['-c', SHUFFLE_HARNESS], {
    encoding: 'utf8',
    input: library,
    timeout: 5_000,
  })
  assert.equal(output, 'ok\n')
})
