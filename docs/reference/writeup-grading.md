# Private writeup grading

Open **Admin → Games → an event → Writeups**. The review lists every accepted
team on the official scoring roster, including teams that have not uploaded a PDF.

1. Select a team. Read its PDF beside the challenge list, or open the download.
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
