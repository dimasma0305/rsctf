# Create and configure games

A game is the event container for teams, divisions, notices, challenges, scoreboards, submissions, and writeups. Challenge type determines the play format, so one game can mix Jeopardy, Attack & Defense, and King of the Hill challenges.

## Core schedule and visibility

Set the title, start time, end time, summary, content, and poster. Keep an unfinished game hidden. Verify the rendered public page with a non-admin account; administrator access can hide visibility mistakes.

## Participation policy

Decide these settings before sharing the join link:

- Minimum and maximum team size
- Game invite code, if any
- Whether accepted teams need manual review
- Divisions and their permissions
- Maximum containers or other resource limits
- Whether practice access continues outside the scored period

When a participation becomes accepted, rsctf can lock the team's roster. Late roster changes may therefore require an administrator action.

## Divisions

Use divisions when groups need separate eligibility, rankings, review rules, or access. Create divisions before teams join, document which division each team should choose, and test one join in every permission combination.

Finalize division scoring permissions and assignments before the event. Once
the scheduled competition begins or durable scoring evidence exists, RSCTF
rejects division creation/deletion, scoring-permission changes, and team moves
that would reinterpret scores. Names and invite-only metadata remain editable
when they do not affect scoring.

## Scoreboard and submissions

Configure whether the scoreboard and submission views are available and when the public scoreboard freezes. A freeze hides recent public results; it does not stop grading.

Score behavior also comes from each challenge: initial score, minimum score,
decay method, and blood bonuses. RSCTF locks these inputs, challenge scoring
eligibility, accepted flags and dynamic flag templates,
schedule/practice/blood settings, and format cadence at the competition
boundary. Configure and rehearse them before start; there is no formula-version
selector or live scoring-policy override.

When at least two challenge formats are active, the public scoreboard opens on an **Overall** tab. RSCTF normalizes each format to 0-100 and gives it one fixed budget unit per enabled, approved challenge. Jeopardy is divided by the attainable score allowed by the team's division, including blood-bonus headroom; A&D and KotH use their official settled epoch totals. Dynamic Jeopardy values stay inside the Jeopardy component and never alter its outer challenge count. Challenge eligibility and counts lock at the competition boundary, and the formula is absolute rather than leader-relative, so field composition cannot rescale a team's result. See the [Overall scoreboard guide](../players/overall-scoreboard).

## Back up and restore competition data

Two buttons move one event's results between installations: **Export Data**
on the game's Info page (game managers and administrators), and **Import
Data** on the admin games list (administrators only). Export produces
`game-{id}-data.zip`; Import accepts that archive and creates a new hidden game
from it.

The archive contains the game definition, its attachments and writeups, the
roster (teams, members, participations), every Jeopardy record (submissions,
first solves, events, notices, reviews, writeup grades), every A&D and KotH
record and rollup (rounds, flags, attacks, checks, cycles, tokens), cheat and
anti-cheat evidence, and telemetry. Each table is written as JSON Lines under
`data/`. Rendered scoreboards are stored under `scoreboards/`, and
`manifest.json` records the row count of every table.

The archive deliberately excludes password hashes, security stamps, VPN private
keys, team and observer API tokens, lease tokens, and container references.
An archive is therefore not a credential backup and cannot revive a running
instance.

Restore semantics:

- Import always creates a **new hidden game**. It never overwrites an existing
  game, so a restore can be inspected before anyone else sees it.
- Import is only allowed for events whose end time has passed. An archive of
  a running or future event is rejected.
- Users are matched by id, then by email, then by username. Unmatched users are
  created as placeholder accounts that keep their archived id and profile but
  have no password; they regain access through password reset. Matched teams
  gain any archived member they were missing.
- Teams are matched by identical id and name; otherwise they are created.
- Participation tokens are regenerated and solve counts are recomputed after
  the rows are restored. Competitive admission timestamps are assigned by the
  database and stay empty on the restored copy; the archive keeps the original
  values for reference.
- VPN telemetry, build records, challenge variants, solve-receipt audit rows,
  and flag-delivery results are exported for reference but not restored.
- Live hill indicators on the KotH board (current container, latest checker
  verdict, reset phase) are not restored because container references are
  never archived; every score, epoch, and rollup is.

Bounds: at most 500,000 rows per table; the uploaded archive is limited to
64 MiB and to 256 MiB when expanded; attachments plus writeups are limited to
128 MiB. When the attachment files exceed that limit, Export Data offers to
export without them (`?attachments=skip` on the API): every score and record is
still included and the attachment metadata is kept, but the files are empty
after a restore. Keep the challenge files in their repository or take a
storage backup alongside.

Use a PostgreSQL dump for full-platform disaster recovery (see
[Back up and update](../deploy/operations)). Use this archive for per-event
backups and for moving one event between installations.

## Discord blood announcements

Set the game's Discord webhook to an official HTTPS `discord.com/api/webhooks/...`
URL to announce first, second, and third blood for Jeopardy challenges. Turning
off a challenge's blood bonus removes only the extra points; the blood badge and
announcement remain active. Announcements reached during a scoreboard freeze
stay queued until the game ends so the webhook cannot leak frozen solves. The
delivery worker retries temporary Discord failures with a bounded backoff, but
does not replay bloods that occurred before webhook delivery was enabled.

## Writeups

If writeups are required, set the deadline and explain the accepted format to players. The current server accepts one lowercase `.pdf` per team, up to 20 MiB; a replacement upload overwrites the previous submission.

## A&D and KotH timing

For games containing A&D or KotH, also review:

- Warmup duration
- Tick/round duration
- A&D flag lifetime
- Service reset cooldown
- Checker/getflag timing and grace periods
- KotH epoch length, crown-cycle length, champion cooldown, claim-confirmation length, and bounded hill weights
- Snapshot and retention behavior

The KotH defaults are a 12-tick epoch, three-tick crown cycle, one-tick previous-champion cooldown, and two consecutive healthy checks for claim confirmation. The epoch must divide cleanly into crown cycles. Official scoring snapshots the cadence, roster, hills, images, and weights and uses one constant formula per format, so settle these values before starting it. Champion cooldown requires enforceable per-hill VPN/firewall isolation; do not enable official crown scoring for an external target where that isolation cannot be enforced.

Run at least several accelerated test ticks with two test teams. Verify flag rotation, target visibility, checker results, provisional-to-confirmed capture, pristine same-image crown reset, exact replacement identity, old-capability rejection, champion cooldown, scoring, and crash recovery before restoring production timing.

## Managers and monitors

Give the smallest role that supports the person's job. Game managers can operate their assigned games; monitors can observe sensitive event data; platform administrators can change global configuration and users. Revoke temporary access after the event.
