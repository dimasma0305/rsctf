# King of the Hill

RSCTF supports two KotH formats. Read the challenge instructions and the format
badge before playing:

- **Boot2root marker KotH** has one shared machine and one holder at a time.
- **API arena KotH** lets every team play and score in the same tick.

For the complete formulas, worked examples, fault rules, and organizer
guidance, read the [KotH scoring handbook](/players/koth-scoring-handbook) or
[download the journal PDF](/downloads/king-of-the-hill-scoring-handbook.pdf).

The proposed next taxonomy separates **Boot2Root KotH** from the
transport-neutral **Leaderboard KotH** format. Its sustained-first-place
formula deliberately removes failed hacking attempts from score penalties.
Read the [two-format design proposal](/players/koth-formats) or
[download its A4 PDF](/downloads/king-of-the-hill-format-design.pdf). The
proposal is not the live scoring contract until its status notice says
otherwise.

## Boot2root marker KotH

Exploit or administer the shared hill and place your current per-hill
capability in:

```text
/koth/king
```

The first healthy observation creates a provisional claim. The same capability
must remain present and the service must remain healthy for the configured
confirmation streak before the team becomes confirmed king.

The platform aggregates three rates for each team, hill, and epoch:

- **Acquisition (`A`)**: eligible crown-cycle windows in which your claim was
  confirmed.
- **Control (`C`)**: personally eligible scorable ticks your capability
  controlled.
- **Reliability (`R`)**: healthy ticks divided by ticks for which your team was
  responsible.

```text
Core  = 0.25A + 0.55C + 0.20 * sqrt(A * C)
Local = 100 * R * Core
```

Control matters most directly, the balance term rewards taking and retaining
the hill, and reliability constrains the whole score.

## API arena KotH

Follow the application's published API or gameplay mechanic. You still use
your current per-hill capability to identify your team to the challenge, but
you do not call RSCTF's signed referee endpoint. The organizer's independent
referee converts verified challenge events into bounded evidence.

Every scorable tick has three normalized rates:

- **Activity (`E`)**: meaningful verified actions divided by the published
  activity target.
- **Objective performance (`P`)**: the equal-weight mean of the challenge's
  independently normalized objective ratios.
- **Integrity (`I`)**: valid actions divided by all counted actions.

```text
B = 0                                  if E = 0 or P = 0
B = 1 / (0.35 / E + 0.65 / P)         otherwise

Tick score = 100 * I * B
```

The harmonic mean requires both play and performance. Guessing or malformed
actions lower integrity. RSCTF calculates each tick before averaging the epoch,
so evidence from separate moments cannot be combined into a result that never
occurred. If your team produces no current-tick evidence, that tick is an
explicit zero; an earlier score is never carried forward.

An API arena has no exclusive holder, provisional crown, or champion-cooldown
score. Several teams can earn points simultaneously.

## Ticks, clean resets, and epochs

The checker samples every hill once per scorable tick at a
server-randomized time. Do not rely on a round boundary as the check time.
Several ticks form a crown cycle. At its boundary, RSCTF pauses the hill,
finalizes evidence, destroys the old container, creates one pristine
replacement from the snapshotted image, revokes old capabilities, runs
readiness, and resumes only after the replacement works.

The reset makes old capabilities, sessions, patches, and implants invalid.
Reset/readiness time, incomplete capability issuance, and platform-attributed
failures are void rather than charged to teams.

Several crown cycles form an epoch. Complete evidence-bearing epochs have equal
weight. A shortened final epoch has proportional weight, and a wholly
field-void hill is omitted from hill normalization. Bounded hill weights never
raise the epoch ceiling above 100.

## Get and protect your capability

1. Open the KotH toolkit.
2. Copy the current capability for the specific hill.
3. Use only the challenge's published marker or application endpoint.
4. Re-fetch the capability after every reset.
5. Keep it out of logs, screenshots, writeups, and public automation output.

Capabilities are bearer secrets bound to one hill, target, container, crown
cycle, and reset attempt. A stale capability cannot score on the replacement.
A ban, team deletion, or invalid roster removes live access; the frozen
official roster remains as historical scoring identity.

## Read the scoreboard

The per-hill badge identifies **Marker** or **API arena**:

- marker hills show Acquisition, Control, Reliability, confirmed king, and
  provisional progress;
- API arenas show Activity, Objective, and Integrity and do not show a crown
  holder.

**Projected** includes unfinished evidence and can change. **Settled** includes
only finalized epochs and determines official rank.

![Live RSCTF crown-cycle scoring board showing a provisional claim, projected points, and acquisition, control, and reliability rates](/screenshots/koth-scoreboard-desktop.png)

*Marker-KotH scoreboard captured from the deployed Docker Compose platform on
13 July 2026. The provisional claim and score evidence are real application
state, not a mockup.*

## Practical strategy

For marker KotH:

- automate capability fetch, exploitation, claim placement, and health checks;
- keep the exact capability stable until confirmation;
- make reversible patches and prepare to reapply them after each clean reset;
- avoid changes that break the checked service for everyone.

For API arenas:

- automate the documented challenge interaction, not the organizer referee;
- complete enough distinct verified actions to satisfy activity;
- optimize every published objective rather than farming the largest native
  number;
- solve server-issued tasks locally instead of guessing against the endpoint;
- monitor per-tick evidence because omitted or late work does not carry.

For both formats, respect the allowed network scope and rate limits, protect
credentials, and verify the scoreboard rather than assuming an action was
sampled.
