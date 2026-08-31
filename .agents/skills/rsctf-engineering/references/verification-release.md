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
scripts/bounded-cargo.sh build --all-targets
scripts/bounded-cargo.sh test
find src -name '*.rs' -print0 | xargs -0 wc -l | awk '$1>1000 && $2!="total"'
scripts/bounded-frontend.sh check
scripts/bounded-frontend.sh lint:check
scripts/bounded-frontend.sh test
scripts/bounded-frontend.sh build
```

`cargo build` must finish with zero errors and zero warnings. Keep focused test output
in the handoff, including expected environment-only skips or ignored tests.

Complete the local-first batch and, when deployed behavior matters, the scoped
`dev.1pc.tf` preview from `fast-development.md` before starting a GitHub workflow.
Do not use repeated pushes as the edit-test loop.

The bounded wrappers are mandatory for local focused and full Rust and frontend checks.
They serialize every worktree on one Git-common-dir lock. Cargo reuses one target
directory, defaults to two Cargo/Rayon workers, and is capped at 200% CPU and 12 GiB
RAM. Frontend work defaults to two Go/libuv workers and is capped at 150% CPU and 8 GiB
RAM. Override those bounds only for an explicitly isolated build host. When `sccache`
is installed, Cargo uses a bounded shared cache automatically. CI may use an equivalent
stricter container/cgroup instead.

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
