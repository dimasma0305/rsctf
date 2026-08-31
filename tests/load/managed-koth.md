# Managed TargetReporter KotH acceptance

`npm run managed-koth` provisions one hidden, paused, disposable Leaderboard
KotH event and exercises the reporter embedded in its platform-managed target.
It does not send observations with the legacy external observer credential.
The observer configuration API is used only to select the event's `Api` scoring
contract; the lifecycle then injects a short-lived `koth_target_…` credential,
context URL, and observation URL into the replacement target.

The gate freezes exactly 2,000 accepted teams. Every capability is authenticated
at 100 fixed arrivals per second, while exactly 64 submitted positive scores form
the bounded score-bearing cohort. Each reporter request contains one finalized
2,000-row wave: 64 positive rows, 1,936 explicit zero rows, and one Crown chosen
from the unique highest submitted score. A later round carries the next wave in
a separate request, so no body exceeds the 2,000 team-wave or 512 KiB limit.

The scenario also:

- checks the initial target remains healthy before reporter variables exist;
- validates the exact six injected variables without printing their secret;
- verifies `Cache-Control: no-store`, API-version `Vary`, objective-schema
  freezing, and every acknowledgement lifecycle field;
- restarts the target process while scoring is paused and proves it reconstructs
  the exact append-only wave prefix;
- suspends and reinstates one participation while scoring is paused, checks one
  capability-generation advance, and permanently rejects the old token;
- recovers the stopped target to a new dynamic address and reset attempt, then
  proves the old target reporter signature returns HTTP 401;
- sends 200 invalid capability attempts per second for 30 seconds, requires both
  HTTP 401 and HTTP 429 with a positive `Retry-After`, and proves reporter
  callbacks remain healthy afterward; and
- requires zero 5xx responses, dropped arrivals, malformed responses, missing or
  duplicate dense rows, exclusive-holder evidence, Crown mismatches, or pending
  revocations.

Run it only on the marked local acceptance Compose project. The runner refuses a
non-loopback origin, an unmarked PostgreSQL/Redis/server topology, a visible
event, missing retained-artifact paths, or an admission profile other than the
deliberate isolated value of 3,000 per minute:

```sh
MANAGED_KOTH_STRESS_ACK=1 \
MANAGED_KOTH_DISPOSABLE=1 \
ADMIN_LIFECYCLE_STACK_MARKER="$ADMIN_LIFECYCLE_STACK_MARKER" \
TARGET=http://127.0.0.1:8080 \
SUMMARY_JSON=/tmp/managed-koth.json \
RESOURCE_JSON=/tmp/managed-koth-resources.json \
npm run managed-koth
```

Start that disposable stack with
`RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE=3000`. This is intentionally
lower than the production default of 6,000 per minute: the default supplies a
6,000-request burst and refills at the legitimate 100-per-second profile. The
isolated 3,000 limit refills at 50 per second, allowing the later 200-per-second
abuse phase to prove admission without changing production defaults.

Capability files live only in a mode-0600 temporary directory and are removed
during cleanup. Reporter secrets stay in process memory, are never written to an
artifact, and are never included in errors or console output. The retained k6
summaries are the requested path plus `-prefix`, `-restart`, and `-abuse`
siblings; the resource file records one-second Docker CPU/RSS samples for the
platform, lifecycle owner, PostgreSQL, and current target during every phase.

This repository batch defines and unit-tests the contract only. A reportable
capacity claim still requires running the command on the isolated stack and
retaining all four summaries and the matching resource series.

The manual **Managed KotH load gate** workflow provides that isolated stack on
a disposable GitHub-hosted runner. Supply the exact current `main` commit and
its immutable `ghcr.io/dimasma0305/rsctf@sha256:…` image. The workflow rejects
a stale source, a mutable or foreign image, an untrusted attestation, the wrong
image revision/version/platform set, and a non-private stack. It bootstraps only
one temporary administrator, keeps the event hidden and paused while preparing
the exact 2,000-team roster, uploads the four k6 summaries plus the resource
series, then removes every run-scoped managed container and Compose volume.

The same workflow runs once when its workflow, Compose overlay, or this runbook
changes in a same-repository pull request. That bootstrap run deliberately tests
the pull request's exact `main` base image rather than building unreviewed code.
