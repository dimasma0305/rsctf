# A&D scoring simulator

This deterministic simulator evaluates rsctf's deployed official
`EpochBalanced` policy against arithmetic, rarity, and legacy controls:

- retired fixed-pot transfer (`gamma=0`)
- square-root conserved transfer (`gamma=0.5`)
- flat conserved transfer (`gamma=1`)
- conserved, field-normalized no-rarity coverage
- bounded, conserved scarcity transfer (`X=0.45*(k/M)*(2-k/M)`)
- maturity-gated scarcity, with negative and positive defense variants
- `manual-equal-arithmetic`, the offline 50/50 governance control
- `manual-equal-balanced`, the deployed official 40/40/20 policy
- iCTF-style fixed service pot
- active-attacker matrix and bounded coverage controls

Run:

```sh
node tools/ad-scoring-sim/test.mjs
node tools/ad-scoring-sim/simulate.mjs
node tools/ad-scoring-sim/simulate.mjs --check
python3 tools/ad-scoring-sim/plot.py
```

`simulate.mjs` writes `results.json` and `REPORT.md`; `--check` fails when
either generated file is stale. The seed and every synthetic profile assumption
are in `simulate.mjs`; no package install or live database is required.

`results.json` schema version 5 uses the epoch outcome field names. The tracked
run uses 20 teams, six equal-budget epochs, and eight ticks per epoch. The main graph at
`graphs/synthetic-20-team-epoch-comebacks.png` summarizes the official evidence
boundary, pairwise defense, local SLA, balance term, and equal epoch totals.

## Official evidence boundary

Teams build, manage, and run their own exploit tooling. rsctf scores the flags
they submit through the normal UI or game-scoped submission API; it does not
need their exploit implementation.

Production declares the global `startRound` only when at least two accepted
teams have every enabled A&D service and every enabled A&D challenge has a
prepared exact custom checker. It freezes the ranked team-service roster from
the flags minted in that round. The simulator represents that roster with one
stable `teamCount` across all rounds and epochs; identities absent from the
snapshot, and captures attributed to them, do not enter scoring.

For one offense-eligible rotating flag:

- `M=N-1` is the number of opponents frozen for that flag.
- `k` is the number of distinct teams with an accepted capture.
- An exact healthy custom check or any accepted frozen-roster capture qualifies
  offense reachability, then every frozen opponent receives the same attack
  opportunity except the flag owner.
- Every capturer records one capture.
- When `M>=4`, every capturer also records rarity fraction `(M-k)/M`.
- Only an exact healthy custom check gives the victim `M` pairwise defense
  opportunities and `M-k` protected opportunities.

A fallback TCP reachability probe does not qualify defense. An accepted capture
can preserve offense evidence during a checker failure, but cannot create
defense evidence.

Across one team-service epoch:

```text
C = accepted captures / attack opportunities
H = sum((M-k)/M for each accepted capture, only when M>=4)
    / attack opportunities
A = min(1, C + 0.25*H)

D = protected defense opportunities / defense opportunities
R = checker credit / eligible service ticks

Arithmetic Core = 0.5*A + 0.5*D
Balanced Core   = 0.4*A + 0.4*D + 0.2*sqrt(A*D)
Local           = 100*R*Core
```

The `0.25` rarity coefficient adds at most 25% of base capture coverage. After
the `A<=1` clamp, the realized lift is never more than 20 percentage points.
Accepted capturer count is its only input. A rare capture is a difficulty proxy,
not proof that a patch was bypassed. Pairwise defense prevents one rare capture
from erasing the entire flag: with `M=19` and `k=1`, the victim retains `18/19`
of that flag's defense opportunities.

Defense remains observational. An unstolen pair does not prove that another
team attempted an exploit. The withholding control therefore stays prominent:
coordinated teams can leave an ally untouched and inflate its `D`. Event
telemetry and enforcement must monitor this behavior continuously.

## Epoch and total semantics

The scorer aggregates raw opportunities, accepted captures, rarity fractions,
protected pairs, and checker credit over the whole epoch before applying `A`,
`D`, `R`, and the nonlinear balance term once. Averaging completed tick scores
is not equivalent.

Service weights are precommitted in `[0.8,1.2]`, snapshotted with each
flag/round, and normalized into a fixed 100-point epoch ceiling. They are a
modest operator-set adjustment for service sloppability or inherent difficulty,
not dynamic rarity and not a reaction to live team performance. Complete epochs
have weight `1`. Production precommits `n` in `[1,64]` so the unresolved raw
evidence window stays bounded. A live or final partial tail with `r` observed
ticks out of `n` configured ticks has weight `r/n`; recency modes in the
simulator are sensitivity controls, not recommendations.

The first complete ranked team-service roster with prepared exact custom
checkers establishes the published `startRound`. Earlier flags, captures,
protected pairs, and checker credit do not enter either total.

The live scoreboard exposes two totals:

- `settledTotal` is the weighted average of finalized epochs only.
- `projectedTotal` is the weighted average of all current evidence, including
  an open partial tail at weight `r/n`.

During play, an epoch finalizes only after its last flag lifetime closes. Game
end closes and finalizes a partial tail at the same fractional weight, so
settled and projected totals then converge. `results.json` includes an explicit
eight-tick `80,20` control whose one-tick tail has weight `1/8`: during play,
settled/projected are `80/73.3333`; after game end they are
`73.3333/73.3333`.

The list UI retains detail rows for the latest three epochs to stay compact.
Both totals continue to use the complete epoch history, including older epochs
that are no longer expanded in the list.

## Model boundary

The repository database has no representative competitive attack history. At
the tracked audit snapshot, both surviving games had zero attacks. All 1,351
captured flags belong to deleted-game cohorts consistent with lifecycle load
traffic and have exactly one capturer. Attacked cohorts also have zero positive
SLA observations.

The simulator consumes no raw database rows. Observed 5, 8, 60, 250, and
300-team topologies anchor scaling checks, while exploit discovery, AI-assisted
diffusion, target mitigation, availability, and team profiles are synthetic.
The Monte Carlo output is a sensitivity test, not proof that any formula
measures skill.

It also omits delayed submissions across rsctf's five-tick flag lifetime. The
reported epoch ranks are therefore useful for formula comparison, while live
settlement timing must be monitored in a real event.

## Deployed policy

`EpochBalanced`, represented by `manual-equal-balanced`, is the sole official
ranking and award policy. Keep `manual-equal-arithmetic` only as an offline
governance control, and monitor score stability, checker eligibility, submission
load, deliberate withholding, flag sharing, and collusion during each event.

Legacy maturity and positive-defense formulas remain adversarial controls. They
help expose recipient concentration, field-size scaling, and assumptions hidden
by native score units. Do not interpret their qualification context as an input
to the official policy.

SLA uses the frozen service x scoring-round grid, so a missing check row earns
zero rather than shrinking the denominator. On checker `InternalError`, carry
the last scored non-infrastructure credit and effective status after
`startRound`. An isolated first `InternalError` earns zero for that service.
Only when every frozen service for a challenge-round has a first
`InternalError` is the sample void for the full roster as a field-wide checker
outage.

## Primary references

- [OtterSec Save CTFs Fund](https://osec.io/blog/save-ctfs-fund/)
- [FAUST CTF 2025 rules](https://2025.faustctf.net/information/rules/)
- [iCTF 2021 scoring](https://ictf.cs.ucsb.edu/archive/ictf_2021/competition_website/howto.html)
- [ECSC 2025 A&D scoring](https://wiki.ad.ecsc2025.pl/scoring/)
- [DARPA Cyber Grand Challenge](https://www.darpa.mil/research/programs/cyber-grand-challenge)
- [DARPA AI Cyber Challenge](https://www.darpa.mil/research/programs/ai-cyber-challenge)
