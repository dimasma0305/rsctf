# King of the Hill

RSCTF supports two constant KotH formats. Read the format badge and challenge
instructions before playing:

- **Boot2Root KotH** has one shared machine and one confirmed holder at a time.
- **Leaderboard KotH** lets every eligible team play and score in the same
  finalized wave.

For proofs, worked examples, fault rules, and organizer guidance, read the
[KotH scoring handbook](/players/koth-scoring-handbook) or
[download the journal PDF](/downloads/king-of-the-hill-scoring-handbook.pdf).
That paper is the canonical contract for both formats.

## Boot2Root KotH

Exploit or administer the shared hill and place your current per-hill
capability in:

```text
/koth/king
```

The first healthy observation creates a provisional claim. The same capability
must remain present and the service must remain healthy for the configured
confirmation streak before the team becomes confirmed king.

For one team, hill, and epoch:

- `A` is the share of eligible crown-cycle windows acquired;
- `C` is the share of personally eligible scorable ticks controlled; and
- `R` is the healthy share of ticks for which that team was responsible.

```text
Core  = 0.25A + 0.55C + 0.20 * sqrt(A * C)
Local = 100 * R * Core
```

Control matters most, the balance term rewards taking and retaining the hill,
and reliability constrains the entire result.

## Leaderboard KotH

Use the application's published gameplay mechanic. Your current per-hill
capability identifies the team to the challenge; players never call RSCTF's
signed referee endpoint. An independent organizer-controlled referee converts
verified challenge events into bounded evidence.

For team `i` in finalized wave `t`:

- `S_it` is its completed, server-confirmed native result;
- `S*_t` is the best positive completed result in that wave;
- `R_it = (S_it / S*_t)^(3/4)` is relative performance; and
- `K_it` is `1` for the unique Crown and `0` otherwise.

```text
R_it = 0                              if the run is missing or incomplete
R_it = (S_it / S*_t)^(3/4)            otherwise
Wave = 100 * (0.95 * R_it + 0.05 * K_it)
```

The three-quarter-power curve keeps nearby results competitive without sharing
or dividing a fixed point pool. Ten teams with similar scores can all earn
nearly 95 performance points. The Crown is the same thing as first place and
adds five recurring points; there is no separate first-place or streak bonus.
On an exact top-score tie, a participating incumbent keeps the Crown. If the
incumbent did not complete the wave, the earliest server-confirmed tied result
wins. Failed hacking attempts do not subtract points, but every new wave needs
a fresh verified completion.

If your team produces no completed evidence in a finalized wave, that wave is
an explicit zero. Earlier evidence is never carried forward. Leaderboard KotH
has one challenge-native Crown per wave but no Boot2Root marker, provisional
claim, or champion-cooldown score; several teams score at the same time.

## Ticks, pristine resets, and epochs

The checker samples each hill once per scorable tick at a server-randomized
time. Do not rely on the round boundary as the check time. Several ticks form a
crown cycle. At its boundary, RSCTF pauses the hill, finalizes evidence,
destroys the old container, creates a pristine replacement from the official
image snapshot, rotates Boot2Root capabilities, runs readiness, and resumes
only after the replacement works. A Leaderboard capability remains valid
across these resets so the same opaque token can re-enter the replacement.

The reset invalidates Boot2Root capabilities plus transient challenge sessions,
patches, and implants. It does not rotate a Leaderboard token. Reset/readiness
time, incomplete capability issuance, and platform-attributed failures are void
rather than charged to teams.

Several crown cycles form an epoch. Complete evidence-bearing epochs have
equal weight. A shortened final epoch has proportional weight, and a wholly
field-void hill is omitted from hill normalization. Bounded hill weights never
raise the epoch ceiling above 100.

## Get and protect your capability

1. Open the KotH toolkit.
2. Copy the current capability for the specific hill.
3. Use only the challenge's published marker or application endpoint.
4. For Boot2Root, fetch the replacement after every crown-cycle reset. For
   Leaderboard, keep using the event token unless you deliberately rotate it.
5. Keep every capability out of logs, screenshots, writeups, and public
   automation output.

Boot2Root capabilities are bearer secrets bound to one hill, target, container,
crown cycle, and reset attempt. Leaderboard capabilities are bearer secrets
bound to one game, hill, and participation for the event. An explicit
Leaderboard rotation invalidates the previous token immediately and clears
that team's unsettled snapshot row; other teams and settled score remain. A
ban or deleted team removes live access, while the frozen official roster
remains the historical scoring identity.

## Read the scoreboard

The per-hill badge identifies **Boot2Root** or **Leaderboard**:

- Boot2Root shows Acquisition, Control, Reliability, confirmed king, and
  provisional progress.
- Leaderboard shows Completion, normalized Objective performance, and Crown
  share. The Crown is finalized per challenge-native wave rather than exposed
  as an exclusive live holder.

**Projected** includes unfinished evidence and may change. **Settled** includes
only finalized epochs and determines official rank. Rank sorts by settled
points, Control/Objective rate, Reliability/Crown-share rate, then acquisition
windows for Boot2Root or completed waves for Leaderboard, and finally stable
participation ID. The live projection never breaks an official tie. Ranks are
ordinal; the final ID makes the order deterministic.

![Live RSCTF crown-cycle scoring board showing a provisional claim, projected points, and acquisition, control, and reliability rates](/screenshots/koth-scoreboard-desktop.png)

*Boot2Root KotH scoreboard captured from the deployed Docker Compose platform
on 13 July 2026. The provisional claim and score evidence are application
state, not a mockup.*

## Practical strategy

For Boot2Root:

- automate capability fetch, exploitation, marker placement, and health
  checks;
- keep the exact capability stable through confirmation;
- make reversible patches and prepare to reapply them after each reset; and
- avoid changes that break the checked service for everyone.

For Leaderboard:

- automate the documented challenge interaction, not the trusted referee;
- complete a fresh verified run in every wave you want to score;
- optimize the published official result relative to the current field;
- solve server-issued tasks instead of guessing; and
- monitor wave evidence because omitted or late work does not carry.

For both formats, stay within the allowed network scope and rate limits,
protect credentials, and verify the scoreboard rather than assuming an action
was sampled.
