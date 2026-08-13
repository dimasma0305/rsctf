# Overall scoreboard

Games that mix Jeopardy, Attack & Defense, and King of the Hill have an
**Overall** scoreboard. It combines the formats without changing how any
individual format awards points.

## Locked challenge-count normalization

Every active format is mapped to `[0, 100]`. Let `C_J`, `C_A`, and `C_K` be the
numbers of enabled, approved Jeopardy, Attack & Defense, and KotH challenges.
RSCTF gives a format one fixed Overall budget unit per challenge:

```text
Overall = (C_J × J + C_A × A + C_K × K) / (C_J + C_A + C_K)
```

For example, one A&D service and two KotH hills produce these format shares:

```text
A&D  = 1 / 3
KotH = 2 / 3
```

Challenge eligibility, review state, and enabled state are locked once
competition scoring begins. The counts therefore remain constant for the
event. A challenge added later is disabled and cannot enter the score. There is
no version selector, separate Overall-weight setting, leader-relative scaling,
or dependence on live solve counts. A leader joining, leaving, or improving
cannot rescale another team's outer format budget.

RSCTF calculates in fixed units of `0.0001` point. Ratios and the weighted mean
are rounded to the nearest unit, keeping replicas deterministic.

## Jeopardy component

For a challenge with initial score `O`, minimum rate `m`, difficulty `d > 0`,
and `n` eligible distinct solves, its current value is:

```text
value = O                                                        when n <= 1

linear factor      = max(m, 1 - (1-m)(n-1)/d)
logarithmic factor = m + (1-m)/(1 + ln(n)/d)
standard factor    = m + (1-m)exp((1-n)/d)

value = floor(O * selected factor)                              when n > 1
```

The score of every eligible solver uses the challenge's current value, so
dynamic decay applies consistently to earlier and later solves. First-,
second-, and third-blood bonuses multiply that value by the configured tier
factor and use round-half-to-even. A challenge can disable blood bonuses.

For team `i`:

```text
J_i = 100 * min(earned_i, attainable_i) / attainable_i
```

`attainable_i` is the sum of current challenge values the team's division may
score, using the largest allowed blood contribution per challenge as headroom.
If the division cannot earn blood on a challenge, only its base value is in
the ceiling. A zero ceiling produces `J_i = 0`. This prevents blood bonuses
from overflowing 100 while keeping divisions with different eligibility
comparable.

Dynamic values continue to determine how much progress each Jeopardy solve
earns inside `J_i`. They do not alter `C_J`: five Jeopardy challenges always
supply five outer budget units, regardless of their current values or solve
counts.

Jeopardy rank sorts by points descending, then the earlier last
score-eligible solve, then stable team ID. Ranks are ordinal. A solve that is
not score-eligible cannot alter that tie-break.

## Attack & Defense component

`A_i` is the official settled A&D epoch total, already bounded to `[0, 100]`.
Its live value is shown only as a projection. A&D rank sorts by settled total,
projected total, offense, defense, SLA, then stable participation ID; ranks are
ordinal.

## King of the Hill component

`K_i` is the official settled KotH epoch total, already bounded to `[0, 100]`,
whether a hill uses Boot2Root control or Leaderboard evidence. Its live value
is shown only as a projection. The fixed formulas are documented in the
[KotH guide](./koth) and [KotH scoring handbook](./koth-scoring-handbook).

KotH official ties never use the live projection. They sort by settled total,
Control/Objective rate, Reliability/Sustained-lead rate, acquisition-window or
activity-positive-tick count, then stable participation ID; ranks are ordinal.

## Combined official and projected values

For all three formats:

```text
Official Overall_i =
  (C_J J_i + C_A A_i(settled) + C_K K_i(settled))
  / (C_J + C_A + C_K)

Projected Overall_i =
  (C_J J_i + C_A A_i(projected) + C_K K_i(projected))
  / (C_J + C_A + C_K)
```

Formats with zero challenges contribute neither numerator nor denominator.
Official Overall rank uses only the official value. Exact fixed-unit ties share
competition rank (`1, 1, 3`); stable team ID affects display order only. The
projected value does not break an official tie.

## Freeze and finalization

During the configured public freeze interval, from freeze time inclusive until
event end exclusive, all component boards use the same frozen evidence view.
Monitors continue to see live evidence. `isFrozenView` is true only in this
active public-freeze interval.

Event end is an immutable evidence cutoff, not a permanently frozen view. At
end, the final Jeopardy board is revealed and A&D/KotH finish settling their
last eligible epochs. The Overall board reports `fullySettled` only after every
active epoch-scored format has durably settled.

To keep historical results reproducible, RSCTF locks score-affecting event,
challenge, division, and format settings once the scheduled competition begins
or durable score evidence exists. This includes schedule/practice/blood
settings, dynamic-score inputs, challenge eligibility, division scoring
permissions, accepted flags and dynamic flag templates, and A&D/KotH cadence.
Cosmetic metadata that does not affect scoring can still be edited.
