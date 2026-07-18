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

Avoid changing division semantics after many teams are accepted. If a change is unavoidable, announce it and review affected participations before the event resumes.

## Scoreboard and submissions

Configure whether the scoreboard and submission views are available and when the public scoreboard freezes. A freeze hides recent public results; it does not stop grading.

Score behavior also comes from each challenge: initial score, minimum score, decay method, and blood bonuses.

## Writeups

If writeups are required, set the deadline and explain the accepted format to players. The current server accepts one lowercase `.pdf` per team, up to 20 MiB; a replacement upload overwrites the previous submission.

## A&D and KotH timing

For games containing A&D or KotH, also review:

- Warmup duration
- Tick/round duration
- A&D flag lifetime
- Service reset cooldown
- Checker/getflag timing and grace periods
- KotH epoch length, crown-cycle length, champion cooldown, claim-confirmation length, and hill difficulty weights
- Snapshot and retention behavior

The KotH defaults are a 12-tick epoch, three-tick crown cycle, one-tick previous-champion cooldown, and two consecutive healthy checks for claim confirmation. The epoch must divide cleanly into crown cycles. Official scoring snapshots the cadence, roster, hills, images, weights, and formula, so settle these values before starting it. Champion cooldown requires enforceable per-hill VPN/firewall isolation; do not enable official crown scoring for an external target where that isolation cannot be enforced.

Run at least several accelerated test ticks with two test teams. Verify flag rotation, target visibility, checker results, provisional-to-confirmed capture, pristine same-image crown reset, exact replacement identity, old-capability rejection, champion cooldown, scoring, and crash recovery before restoring production timing.

## Managers and monitors

Give the smallest role that supports the person's job. Game managers can operate their assigned games; monitors can observe sensitive event data; platform administrators can change global configuration and users. Revoke temporary access after the event.
