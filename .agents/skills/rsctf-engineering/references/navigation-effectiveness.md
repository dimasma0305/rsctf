# Navigation effectiveness

This is a navigation check, not evidence that application requests run faster or
that a developer finishes a task in a measured amount of time. Source ownership,
permission behavior and the correctness of a fix still need their own review/tests.

## Coverage and useful failures

The first coverage audit found direct links for 12/15 controller families, 46/60
service families, 2/7 hub modules, 2/3 storage modules and 55/56 pages. Those gaps
were fixed. The checker now requires 15/15, 60/60, 7/7, 3/3 and 56/56 respectively
on the measured main baseline. It discovers the denominator from source on each
run, so additions require a corresponding map entry.

Coverage means a direct owner or a specific family directory is linked. A root
`src/` or `web/src/pages/` shortcut does not count for all children. Public and
crate-visible `mod name;` declarations in controller/service/hub/storage roots
are checked; this is not a Rust AST, call-graph, endpoint or permission-coverage
analysis. Frontend page-family coverage does not mean each page has its own
API/test pairing or has been browser-tested by this check.

Negative fixtures verify missing files, checkout escape, absolute paths, invalid
encoding, newly unmapped modules/pages, and overly broad page links are rejected.
Encoded spaces, nested labels, external links, fenced examples and an unrelated
working directory are covered too. CI runs these regressions before checking the
real map. The initial owner-lookup trial also failed for expired-record cleanup:
the map linked the parent module but not its SQL helper directory. Adding that
directory made the expected owner discoverable without a repository-wide search.

## Reproducible owner lookup

Run from the checkout; Node and ripgrep are the only requirements:

```sh
node --test .agents/skills/rsctf-engineering/scripts/check-navigation.test.mjs
node .agents/skills/rsctf-engineering/scripts/check-navigation.mjs
node .agents/skills/rsctf-engineering/scripts/benchmark-navigation.mjs > /tmp/rsctf-navigation.json
```

The [benchmark](../scripts/benchmark-navigation.mjs) uses eleven curated questions:
OAuth cookies, client cache weight, CTFtime, CSRF, durable claims, checker locks,
capture ownership, worker certificates, Kubernetes network policy, workspace UI,
and expired-record cleanup. Each has a source-checked expected owner.

Both sides use the same regex and single-thread ripgrep. Baseline searches cover
source, tests, agents, libraries, scripts and deployment/workflow code. Mapped
searches use owner paths extracted from the selected table row. Each case has one
excluded warmup pair and eleven alternating measured pairs. The retained
[raw results](navigation-benchmark.json) include every timing sample, source hashes,
expected owners, scopes, hardware and tool versions. `sourceRevision` records the
base commit while the doc changes were uncommitted; the hashes identify the exact
measured maps and benchmark script.

Across the eleven cases, candidate task/file pairs fell from **161 to 39 (75.8%)**,
with all eleven expected owners preserved. These are summed per-task counts,
not 161 unique files. The broad search universe contained 1,976 code files.
The durable-job case stayed at two candidates: maps do not always reduce results.
Timing values are mechanical search costs available in the raw results.

Rows and expected owners were curated, not selected in a blind evaluation.
Question interpretation, choosing/reading a row, reasoning, editing and testing
are excluded from timings. Baseline search is deliberately broad; an experienced
developer can already narrow it. Excluded callers/tests may still be relevant.
The demonstrated benefit is less first-owner search noise with checked ownership
retention; no human productivity multiplier or bug-finding rate is established.

Rerun after structural changes. A green coverage check can still coexist with an
incorrect explanation or an overly broad family entry, so use real lookup tasks
and source review alongside the automated gate.
