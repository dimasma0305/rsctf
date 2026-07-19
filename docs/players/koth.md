# King of the Hill

In King of the Hill (KotH), teams compete for control of one shared service called the hill. The first healthy checker observation of a valid team capability creates a provisional claim; the same team and capability must remain in control for the configured number of consecutive healthy checks before that team becomes the confirmed king.

For the complete formula, worked examples, failure rules, and organizer guidance,
read the [KotH scoring handbook](/players/koth-scoring-handbook) or
[download the journal PDF](/downloads/king-of-the-hill-scoring-handbook.pdf).

## The objective

Exploit or administer the hill, then write your team's current hill capability to the marker described by the challenge. The standard marker is:

```text
/koth/king
```

The challenge instructions are authoritative if they specify a different mechanism.

## Capture the hill

1. Open the KotH toolkit in the game.
2. Copy the current capability for the specific hill.
3. Connect to the displayed hill endpoint through the required network path.
4. Gain the access required by the challenge.
5. Write only the capability, without extra formatting, to the marker.
6. Keep the service healthy and the capability in place until the claim is confirmed.

Capabilities are bound to a hill, its exact container, and the current crown-cycle reset. Re-read the toolkit after every reset; a capability from an earlier cycle cannot claim the replacement container.

Capability access is also bound to the team's current eligible roster. A ban,
team deletion, or missing account revokes the team's live bearer credentials and
holder projection. The official scoring roster remains frozen for historical
identity and denominators; only currently eligible teams receive or can present
new cycle capabilities, so removing one team does not void the remaining field.

## Ticks, crown cycles, and epochs

The checker observes every hill once per scorable tick at a server-randomized time. Do not rely on the round boundary as the check time. Several ticks form a **crown cycle**. At every cycle boundary, rsctf pauses scoring, finalizes the cycle, destroys the old container, and creates exactly one replacement from the same pristine challenge image. It clears the holder and provisional claim, revokes old capabilities, waits for the replacement to pass readiness, applies any champion cooldown, and only then starts the next cycle. Reset and readiness time is not scoring evidence.

Several crown cycles form a scoring **epoch**. The defaults are 12 ticks per epoch, three ticks per crown cycle, a one-tick previous-champion cooldown, and two consecutive healthy checks to confirm a claim. Organizers may choose another valid cadence, and the values are snapshotted when official scoring starts, so use the live toolkit and scoreboard rather than hard-coding the defaults.

## Scoring behavior

The platform aggregates three rates for each team, hill, and epoch:

- **Acquisition (`A`)** is your share of personally eligible crown-cycle capability windows in which a provisional claim becomes confirmed. A team can earn acquisition credit only once per hill and capability window.
- **Control (`C`)** is your share of personally eligible scorable ticks on that hill. A provisional claimant receives control evidence immediately when the checker observes its exact current capability.
- **Reliability (`R`)** is healthy responsible ticks divided by responsible ticks. Provisional control receives responsibility evidence immediately. A team with no responsibility has zero reliability and earns no points.

The local score is bounded from 0 to 100:

```text
Core  = 0.25A + 0.55C + 0.20 * sqrt(A * C)
Local = 100 * R * Core
```

Sustained control has more direct weight than acquisition speed, while the square-root term rewards teams that do both. Reliability multiplies the complete core: a team cannot protect a strong control score while leaving the service broken.

There are no negative point deductions and no new-holder grace tick. A different valid capability, `Mumble`, or `Offline` breaks a provisional confirmation streak; unhealthy responsible ticks also lower `R`. An `InternalError`, reset/readiness time, or incomplete capability issuance is void evidence rather than being charged to a team.

The previous cycle's champion is the team with the most confirmed healthy controlled ticks, not merely the final holder. That champion is normally blocked from this hill for the configured opening tick after readiness. Every tied leader is cooled down unless that would leave no eligible challenger. The forced cooldown tick is removed from the affected team's personal scorable-tick denominator; the team may compete again for the rest of the cycle.

Destroying a platform-managed hill during an active cycle does not erase responsibility. A confirmed container death is attributed as `Offline` to the responsible team before the durable recovery path replaces it; a container-runtime inspection failure is not proof of team-caused downtime. Planned cycle resets clear holder and responsibility state after finalization.

Hill difficulty weights are bounded and normalized, so adding a hill or changing its approved weight does not raise the epoch's 100-point ceiling. A hill with no field-wide scorable evidence is omitted from that epoch's hill normalization instead of acting as a zero score for every team. Any hill with at least one scorable tick keeps its full approved weight; individual void samples are already excluded from personal denominators.

## Epoch totals

Every finalized, evidence-bearing complete epoch has equal weight. Field-wide reset/readiness time and platform-attributed failures are excluded from scoring denominators, and an entirely field-void epoch has zero weight. A partial final epoch contributes in proportion to its played scorable ticks compared with its expected scorable ticks. There is no late-epoch multiplier.

The **Live** value includes the open epoch as a projection and can change as checks arrive. The **Settled** value includes only finalized epochs and determines rank. Equal Settled scores are resolved by control rate, reliability, confirmed acquisition windows, and finally participation ID. These keys produce one ordinal place per team; the projection never breaks an official tie.

## Read the scoreboard

The desktop board groups four columns under each hill: settled hill score,
acquisition, control, and reliability. A status badge and cell tint show the
latest functional verdict. The crown marks the confirmed king; a separate
indicator shows a provisional claimant and confirmation progress. The board
also reports the crown-cycle number and tick, reset/readiness phase, next reset,
and any active champion cooldown.

![Live RSCTF crown-cycle scoring board showing a provisional claim, projected points, and acquisition, control, and reliability rates](/screenshots/koth-scoreboard-desktop.png)

*Fixed-formula scoreboard captured from the deployed Docker Compose platform on 13 July 2026. The provisional claim and score evidence are real application state, not a mockup.*

Select the information icon beside the board title for the fixed formula,
snapshotted cadence, reset lifecycle, cooldown, and Live-versus-Settled explanation.
On a narrow screen, the ranking keeps rank, team, and Settled visible; selecting
a team opens its per-hill score and raw evidence counts.

## Practical strategy

- Automate the per-hill capability fetch and marker write, but respect rate limits.
- Keep the current capability planted and preserve the service behavior the checker expects until confirmation.
- Fetch the new capability and exact endpoint after every crown-cycle reset.
- Prepare automation to re-exploit and reapply patches: the replacement is pristine, so patches survive only until the next reset.
- Monitor scoreboard evidence rather than assuming a successful write was observed.
- Avoid destructive changes that make the hill unavailable to everyone.
- Keep credentials and tokens inside your team.
