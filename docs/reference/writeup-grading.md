# Private writeup grading

Open **Admin → Games → an event → Writeups**. The review lists every accepted
team on the official scoring roster, including teams that have not uploaded a PDF.

1. Select a team. Read its PDF beside the challenge list, or open the download.
   Use the page controls above the preview to move through the document. Only the
   selected page is rendered; switching to the projected scoreboard releases the
   canvas while retaining the loaded document, selected page and unsaved grades.
2. Enter a whole-number grade from **0 to 100** for each scored challenge and
   select **Save grade**. For example, a 500-point contribution graded 80% retains
   400 points in the private projection.
3. Open **Projected scoreboard** to compare original and graded points. Select a
   score format or division as needed. Select a team name to return to its review.

Ungraded challenges retain 100%; they do not count as reviewed. **Mark ungraded**
clears a saved grade. A missing writeup does not automatically deduct points; an
admin can explicitly enter 0%. Changes are saved per challenge, not on every keypress.

Jeopardy contributions include dynamic scoring and applicable blood bonuses.
A&D and KoTH contributions use finalized epochs. Overall retains RSCTF's existing
format weights and normalization; raw points from different formats are not added
together. The private ranking uses competition-style ties (1, 1, 3) and preserves
overall/division ranking eligibility. It is a review projection, not an award result.

The page does not poll. **Refresh scores & grades** loads the current scoring
snapshot and other administrators' edits. An unfinished epoch is identified as
provisional. If an edit conflicts, refresh before changing that grade again.
Unsaved input is not part of the ranking. A replacement PDF does not clear grades;
reviewers should revisit those challenge grades after replacing a submission.

## Isolation and API

Only platform administrators can read or change grades. Event managers, monitors,
players and anonymous visitors cannot access the grading endpoints. Public
scoreboards, solve records, epoch results and awarded competition points are never
changed. The endpoints return `Cache-Control: private, no-store`.

- `GET /api/admin/writeups/{gameId}/grading` returns accepted teams, current
  writeup URLs, scored challenges, original contributions and saved grades.
- `PUT /api/admin/writeups/{gameId}/grading/{participationId}/{challengeId}` accepts
  `{ "percentage": 80, "expectedRevision": 0, "operationId": "<UUID>" }`.
  `percentage: null` clears a grade. Use the returned revision on the next edit.
  Reuse the same operation ID and body when retrying an uncertain request.

Only scored challenges belonging to the selected event/team can be graded. A
revision conflict returns 409 rather than overwriting another reviewer. The private
table is separate from scoring and is removed by normal parent-record cascades.
Complete projections are limited to 10,000 teams and 100,000 team/challenge pairs;
oversized events receive an explicit error instead of a partial ranking.

## Browser rendering budget and verification

The preview mounts one `Page` at most and no pages when its review tab is hidden.
It caps canvas pixel density at 2×, retains the last nonzero layout width while
hidden, and keeps the text layer available for selection and assistive technology.
The original PDF download remains unchanged. This bounds simultaneous page
rendering; it does not claim a fixed total-memory limit for arbitrary PDF content.

Run `tests/visual/writeup-pdf-performance.mjs` against a local production-build
preview on port 63017. It intercepts API/PDF requests with synthetic fixtures:

```sh
RSCTF_FRONTEND_CPU_QUOTA=100% scripts/bounded-frontend.sh exec node ../tests/visual/writeup-pdf-performance.mjs
RSCTF_FRONTEND_CPU_QUOTA=100% scripts/bounded-frontend.sh exec node ../tests/visual/writeup-grading.mjs
```

Use `--baseline` on the performance harness to record a prior build without the new
rendering-budget assertions. `RSCTF_WRITEUP_OUTPUT` selects a distinct output
directory. The fixed workload is 60 portrait pages at 1440×1100 / 2× density,
12 tab changes scheduled 750 ms apart, and the same one-core bounded runner.
Reports contain avg/p50/p90/p95/p99/max input delays, long tasks, scheduling lag,
renderer task time, canvas storage estimates and error/request counts. They measure
browser behavior, not backend throughput; no real grades or scoreboard data change.

### Rendering optimization record — 2026-09-08

Same host, workload and runner limit; prior build `e2189ed9` versus the paged preview:

| Measurement | Previous preview | Paged preview |
| --- | ---: | ---: |
| Peak mounted canvases | 60 | 1 |
| Peak canvas backing-store estimate | 876.4 MiB | 5.4 MiB |
| Canvas width/height changes | 1,560 | 14 |
| Renderer task time | 21.34 s | 3.65 s |
| Tab response p95 | 3,671 ms | 288 ms |
| Maximum arrival lag | 6,161 ms | 4 ms |

Both runs downloaded the PDF once and had zero unexpected API calls or runtime
errors. These are synthetic measurements on this host, not guarantees for every
device or document. The regression gates require at most one canvas, under 16 MiB
of canvas buffers for this fixture, sub-second tab responses, and keeping up with
the scheduled input rate. The interaction harness also verifies page navigation,
hidden-canvas release, draft/page retention, five responsive/theme layouts and Axe.
