# Verification, release, and production

## Evidence by risk

- Backend logic: focused unit/integration test plus `cargo test`.
- SQL/authorization: exercise a real disposable PostgreSQL database and include
  negative visibility/permission cases.
- Frontend: strict typecheck, lint, client tests, production build, and visual/Axe
  audit for changed screens.
- Containers/workers/installers: exercise the real lifecycle or preflight, not only a
  mocked DTO.
- Polled reads, A&D/KotH engine, BYOC, or optimization: run the applicable fixed-rate
  load harness and integrity checks from the performance reference.

Before release, require:

```sh
cargo fmt --all -- --check
cargo build --all-targets
cargo test
find src -name '*.rs' -print0 | xargs -0 wc -l | awk '$1>1000 && $2!="total"'
cd web && pnpm check && pnpm lint:check && pnpm test && pnpm build
```

`cargo build` must finish with zero errors and zero warnings. Keep focused test output
in the handoff, including expected environment-only skips or ignored tests.

## Local-first feedback loop

Use GitHub Actions as the final clean-room gate, not as the primary debugger.

1. Reproduce the failure with the smallest focused local test or the exact failing
   harness phase. Add diagnostics locally and keep the evidence that identifies the
   failed invariant.
2. Run the affected workflow in a disposable local environment. For container and
   lifecycle work, use the exact candidate image or digest and preserve the same
   topology, configuration, and release thresholds where the host permits it.
3. When the behavior depends on the deployed proxy, browser, worker, or multi-host
   topology, test it on `https://dev.1pc.tf` in a hidden event that only administrators
   can discover. Keep this test scoped and clean up its disposable resources.
4. Run focused regressions, then the applicable broader local checks. Local diagnostic
   overrides for a contended development host must remain uncommitted; restore and
   verify the release values before staging.
5. Inspect `git diff --cached` and the committed files before pushing. Start the full
   GitHub gate only after the local and, when applicable, dev checks pass.

If a GitHub gate fails, reproduce that failure locally before another code push. A
remote rerun without a code change is appropriate only for a documented external or
runner failure.

## Immutable production release

Any completed change that can affect a built/deployed artifact—Rust, React, agents,
installers, migrations, compose/release configuration, or tooling shipped in the
image—must be built and deployed to `https://tcp.1pc.tf`. Prose, comments, and tests
that cannot alter an artifact may skip deployment; state that explicitly.

1. Run local checks before publishing.
2. Commit and push the intended branch, use the repository release process, and wait
   for every required workflow/image job to succeed.
3. Resolve and deploy the immutable image digest. Do not deploy a local build or trust
   a mutable tag.
4. Roll every applicable web/control replica to the same expected release and digest,
   preserving data/secrets and applying only registered forward migrations.
5. Verify `GET https://tcp.1pc.tf/healthz` is HTTP 200 with exact body `ok`.
6. Verify every production container is healthy and reports the expected version and
   digest. Inspect recent logs for panic, migration failure, restart loop, and
   unexpected 5xx.
7. Smoke-test the exact changed behavior, including negative authorization cases when
   relevant. If a worker/installer changed, fetch both public Linux and Windows
   bootstrap endpoints and exercise their preflight/install regression.

Do not report completion until all live checks pass. The final evidence must state the
release version, immutable digest, replica health, smoke-test results, and log result.
If credentials, artifacts, workflows, or the host block deployment, say production is
not deployed and identify the blocker.
