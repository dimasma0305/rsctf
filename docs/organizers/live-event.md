# Operate a live event

Use a shared runbook and one decision-maker for platform changes. A quiet, predictable response is more valuable than an untested “quick fix” during scoring.

## Before opening

- Confirm `/livez` is responsive and `/healthz` reports dependency readiness.
- Check rsctf, PostgreSQL, and Redis container/Pod status.
- Confirm disk space and the most recent backup.
- Verify event times, visibility, notices, divisions, and accepted teams.
- Test one flag submission and one scoreboard refresh.
- For dynamic challenges, create and destroy one instance.
- For A&D/KotH, verify the current round, checker, VPN route, and targets. For KotH, also verify the active crown cycle, exact container identity, readiness state, per-hill capability issuance, provisional confirmation, and champion cooldown enforcement.
- Open the private organizer and player-support channels.

## What to monitor

Watch the Admin dashboard, logs, instances/builds, pending teams, notices, and per-game event/submission views. At the infrastructure layer, watch:

- rsctf error rate and response latency
- PostgreSQL connections, CPU, storage, and slow queries
- Redis availability and memory
- Host/cluster CPU, memory, disk, and open file limits
- Challenge container/Pod count and failures
- A&D checker duration relative to tick length
- A&D flag-publication failures and publication lag after each round boundary
- round start-to-start cadence (the stored boundary must match the configured tick)

When Redis is configured, its failure makes `/healthz` unready while rsctf keeps
`/livez` responsive and retries the connection. Treat the readiness failure as
an incident; do not route new event traffic to an unready replica.

## Incident sequence

1. **Record the UTC time** and affected game/challenge/team.
2. **Limit the blast radius** by disabling the specific challenge or pausing the relevant operation if possible.
3. **Preserve evidence**: logs, failed endpoint, instance ID, and infrastructure metrics.
4. **Communicate** a short notice without leaking flags or internal details.
5. **Apply a tested recovery** such as restarting one failed instance or rolling back a challenge image.
6. **Verify as a normal player**, then announce recovery and any scoring decision.

Avoid restarting the entire platform for a single broken challenge. A restart can interrupt every session, WebSocket, background task, and active A&D operation.

## Score and integrity checks

During and after the event:

- Review suspicious flag sharing and anti-cheat reports.
- Check that submissions, solves, and score changes agree.
- Confirm A&D rounds and KotH captures are not duplicated.
- Confirm completed A&D rounds have zero unresolved checks, zero missing hill observations, and no scorable evidence timestamped at or after the round end.
- Treat a nonzero `flagDeliveryFailures` value as an incident. `flagsReady` means the bounded publication phase settled; the failure count shows whether every service acknowledged it.
- After a long scheduler outage, expect one visible field-wide time gap. RSCTF re-anchors the next playable round instead of replaying expired rounds and flags back-to-back.
- Confirm each KotH hill has exactly one active container and that reset/readiness rounds produce no scoring evidence.
- Record manual score or participation decisions with an organizer and timestamp.
- Keep the frozen board policy consistent for every team.

## Closing the event

1. Confirm the configured end time has passed.
2. Stop or close late submissions according to policy.
3. Reveal/unfreeze results when intended.
4. Export the score/submission data needed by the organizers.
5. Preserve writeups and uploaded evidence.
6. Take a final PostgreSQL and file-storage backup.
7. Remove temporary admin/monitor access and rotate temporary credentials.
8. Destroy unused challenge instances only after preserving anything the event needs.
