# Fix a live A&D or KotH challenge

Open **Operations → Live challenge updates → Open maintenance window**.

1. Wait for **Scoring is paused**. A&D waits for the current round to finish; KotH waits for the end of its current epoch so a scoring-source change cannot split an epoch.
2. Edit the challenge and save. Build any changed container image successfully. Configure or rotate its API observer if needed.
3. Choose **Apply changes and resume**. The platform retires affected runtimes, records the activation round, and lets the engine provision and check their replacements.

If validation or cleanup fails, scoring stays paused. The panel shows the failure; correct it while editing is available, or retry application after a cleanup failure. Refreshing the page does not lose the maintenance window. A request can be cancelled until the window opens.

| Change | Availability |
| --- | --- |
| Title, description, category, decorative images, branding, announcements and organizer contact | Use the usual editor; no maintenance window needed. |
| Container image, prepared checker, CPU/RAM/storage, runtime profile, supported network/egress settings and port | Edit in the maintenance window. Existing runtime validation still applies. |
| Marker → API observer | Enable the observer during maintenance; its scoring source activates in the next epoch. |
| API observer credential rotation/revocation, restart/recovery, pause/resume, quarantine and diagnostics | Existing operator controls remain available. A maintenance application fences conflicting changes until it finishes. |
| Event end time, late teams, suspension/reinstatement | Existing controls remain available. Late teams receive no credit for missed rounds. |
| Past scores/evidence, scoring start, weights, completed epoch/cycle rules and historical roster | Remain locked. |

A replacement image reference during live maintenance is pulled from that reference. The original uploaded archive remains available for audit; its old Dockerfile is not rebuilt under the replacement name. Prepared checkers must still contain `run.py` on the engine host. Multi-service `workloadSpec` remains a Jeopardy-only feature; A&D and KotH use their supported single-container runtime configuration.

The maintenance history retains before/after definitions and `effectiveFromRound`. KotH retains the earlier image and claim-source revisions, so rebuilding a scoreboard still evaluates each earlier epoch with its original scoring source. A replacement's readiness samples remain platform voids until the normal runtime/checker requirements pass.

Use **Apply changes and resume** to close an open window. The ordinary resume button cannot bypass validation. If scoring was already paused before a request and the current epoch is unfinished, use the ordinary resume control to reach the boundary first. Extend the event end time if the remaining schedule would end before that boundary.

One event can retain up to 128 maintenance windows and contain up to 256 A&D/KotH challenges in this workflow.
