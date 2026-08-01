# Overall scoreboard

Games that mix Jeopardy, Attack & Defense, and King of the Hill have an **Overall** scoreboard. It combines the formats without changing how any individual format awards points.

## Fixed normalization

Every active format is mapped to a score from 0 to 100. Each active format then receives the same constant weight:

- One format: 100% of Overall
- Two formats: 50% each
- Three formats: 33.333% each

The platform does not normalize against the current leader, so field size and the
leader's score do not rescale the board. Jeopardy's existing dynamic challenge
values still behave normally: when eligible solve counts change a challenge's
current value, both earned points and the attainable ceiling are recalculated
from that same snapshot.

For a game with all three formats:

```text
Overall = (Jeopardy normalized + A&D settled + KotH settled) / 3
```

## Format inputs

**Jeopardy** uses the team's earned points divided by the current attainable points for challenges its division may score. The attainable ceiling uses each challenge's current dynamic value and the largest blood bonus the division is allowed to receive. This keeps different raw point scales comparable and prevents bonus points from overflowing 100.

**Attack & Defense** uses the official settled 0-100 epoch total.

**King of the Hill** uses the official settled 0-100 epoch total, whether a hill uses exclusive marker control or normalized API-arena evidence.

Unfinished A&D and KotH epochs appear as an orange **Live** projection. They do not change the official Overall rank until they settle. Exact official ties share a rank; team ID only keeps their display order stable.

## Freeze behavior

The Overall board uses the same public freeze boundary as its component boards. Monitors retain the live view. After the event, the board waits for active epoch-scored formats to report `fullySettled` before presenting the result as final.
