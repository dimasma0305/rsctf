---
title: RSCTF King of the Hill Handbook
description: Canonical scoring and operations contract for RSCTF Boot2Root and Leaderboard King of the Hill.
pageClass: koth-handbook
---

<div class="journal-title-block">
  <p class="journal-series">RSCTF TECHNICAL PRACTICE PAPER</p>
  <h1>RSCTF King of the Hill Handbook: Boot2Root and Leaderboard Formats</h1>
  <p class="journal-authors">Dimas Maulana</p>
  <p class="journal-affiliation">RSCTF Project · Competition Platform</p>
  <p class="journal-correspondence">Unified edition · Version 5.0 · 2 August 2026</p>
  <p class="journal-policy">Scoring policy: one constant formula per KotH format; no scoring versions</p>
</div>

<p class="pdf-download"><strong>Download:</strong> <a href="../downloads/king-of-the-hill-scoring-handbook.pdf" download>King of the Hill handbook (A4 PDF)</a>.</p>

## Abstract

<div class="journal-abstract">
<p>RSCTF defines two King of the Hill (KotH) formats with different competitive meanings. <strong>Boot2Root KotH</strong> is an exclusive-control contest over one shared machine: teams acquire the hill, retain control, and accept responsibility for service health. <strong>Leaderboard KotH</strong> is a concurrent application or protocol contest: every eligible team can play in the same tick, challenge-native outcomes are normalized by RSCTF, and consistently remaining first earns a bounded bonus. Boot2Root uses the constant score 100R(0.25A + 0.55C + 0.20√AC). Leaderboard uses a per-tick harmonic activity/objective performance rate, exact tied-leader credit, and adjacent-tick continuity. Failed hacking attempts are not negative points; authenticity, replay protection, exact runtime identity, independent checking, and challenge-level proof design determine whether evidence is admitted. Both formats use scoped capabilities, pristine crown-cycle replacement, bounded hill aggregation, and finalized epoch settlement. This paper is the canonical deployed scoring and operations contract.</p>
</div>

<p class="journal-keywords"><strong>Keywords:</strong> King of the Hill; Boot2Root; Leaderboard KotH; normalization; sustained lead; anti-cheat; crown cycle; RSCTF</p>

<p class="journal-status"><strong>Document status:</strong> current implementation contract for the repository revision containing this paper. Public names are Boot2Root and Leaderboard; the stable internal claim-source identifiers remain <code>Marker</code> and <code>Api</code>. Those identifiers are not selectable scoring versions. This is a technical-practice report, not a claim of peer review or empirical validation.</p>

<figure class="journal-figure">
  <img src="/diagrams/koth-two-format-model.svg" alt="Two King of the Hill formats: exclusive Boot2Root control and concurrent Leaderboard competition, both entering bounded epoch settlement">
  <figcaption><strong>Figure 1.</strong> Boot2Root measures exclusive machine control. Leaderboard measures normalized concurrent performance and sustained first place.</figcaption>
</figure>

## Start here: KotH in 90 seconds {#start-here}

### Choose the hill's format

**Boot2Root** means one shared machine and one holder:

1. exploit or administer the hill;
2. place the current per-hill capability in `/koth/king`;
3. remain healthy through confirmation; and
4. retain control until takeover or the pristine reset.

**Leaderboard** means every eligible team may score concurrently:

1. use the challenge's player-facing mechanic;
2. complete meaningful, verified work;
3. optimize every published objective; and
4. sustain first place across adjacent ticks.

Players never call the organizer's signed referee endpoint.

### The two constant formulas

```text
BOOT2ROOT
Core  = 0.25A + 0.55C + 0.20 * sqrt(A * C)
Local = 100 * R * Core

LEADERBOARD
Q_t   = 0 if E_t=0 or P_t=0; otherwise 1/(0.35/E_t + 0.65/P_t)
D     = 0.25L + 0.55S + 0.20 * sqrt(L * S)
Local = 100 * [Q + 0.50 * Q * (1-Q) * D]
```

The first formula uses acquisition `A`, control `C`, and reliability `R`. The
second uses tick activity `E_t`, normalized objective performance `P_t`, mean
tick performance `Q`, lead coverage `L`, and sustained-lead continuity `S`.

### What resets and what settles

Several ticks form a **crown cycle**. RSCTF then pauses the hill, destroys the
old container, creates one pristine replacement from the frozen image, issues
new capabilities, verifies readiness, and resumes. Old capabilities, sessions,
patches, implants, and transient referee snapshots stop working.

Several cycles form an **epoch**. **Projected** includes unfinished evidence;
**Settled** includes finalized epochs and determines official rank.

## 1. Scope and design requirements {#introduction}

### 1.1 Why the formats are separate

A Boot2Root hill asks who controls one shared target. Its natural evidence is
exclusive identity, duration, acquisition, and service responsibility. A
concurrent application may instead expose puzzles, transactions, simulations,
or protocol objectives that many teams can complete at once. Treating the
second as a one-holder marker creates last-write races; treating the first as
an external point feed transfers scoring authority away from the platform.

RSCTF therefore freezes one competitive meaning per hill:

<p class="journal-table-caption koth-keep-table"><strong>Table 1.</strong> Fixed public formats and internal claim-source identifiers.</p>

| Public format | Internal source | Concurrent scorers | Evidence |
| --- | --- | ---: | --- |
| Boot2Root | `Marker` | at most one observed controller | acquisition, control, reliability |
| Leaderboard | `Api` | every eligible team | activity, objectives, sustained lead |

An API credential configured before official scoring selects internal source
`Api`; otherwise the hill uses `Marker`. The source cannot change after scoring
begins. “Leaderboard” describes the competition, not its transport: the player
surface may be HTTP, a binary protocol, a simulator, or a game server.

### 1.2 Shared invariants

Both formats preserve these requirements:

1. **Constant policy.** Each format has one formula and fixed coefficients.
2. **Bounded result.** A hill-epoch and aggregate epoch remain in `[0,100]`.
3. **Current identity.** Evidence belongs to the exact game, hill, round,
   cycle, reset attempt, target, container, and capability generation.
4. **Independent health.** A functional checker, not the scoring feed, decides
   whether the shared application works.
5. **No stale carry.** A missing team in a valid Leaderboard snapshot becomes
   zero; a missing field snapshot is void. Neither repeats old evidence.
6. **Infrastructure neutrality.** Reset, readiness, incomplete issuance, and
   attributable platform failures do not penalize participants.
7. **Auditable settlement.** Immutable tick evidence precedes final rollups,
   and retries cannot create duplicate credit.
8. **Frozen bounded state.** Rosters, hills, cadence, images, weights, objective
   identities, request sizes, replay memory, and retained evidence are bounded.

### 1.3 Relation to other systems

Bock, Hughey, and Levin describe an instructional KotH in which taking a
shared machine also creates responsibility for critical services
[[2]](#ref-2). Boot2Root follows that exclusive-control interpretation while
adding periodic pristine replacement and fixed epoch settlement.

CTFd documents a KotH agent that reports a current identifier and awards a
configured reward per check [[3]](#ref-3). RSCTF additionally records
provisional control, confirmation, acquisition, duration, and reliability.

rCTF documents an HMAC-signed dynamic-score endpoint accepting absolute
`userId/points` setters [[5]](#ref-5). RSCTF retains exact-body signing and
replay resistance, but Leaderboard never accepts external points or team IDs.
The platform normalizes bounded evidence, writes omitted-team zeros, detects
leaders, and computes every score itself.

FAUST CTF is an Attack & Defense system rather than a shared hill, but its
repeated rounds and service checks provide an operational comparison
[[4]](#ref-4). Its topology and formula are not RSCTF KotH.

## 2. Competition protocol {#competition-protocol}

### 2.1 Boot2Root protocol

RSCTF runs one managed shared container per Boot2Root hill. Every eligible team
receives a high-entropy bearer capability scoped to the exact hill and reset
attempt. A team claims by writing it to:

```text
/koth/king
```

The checker reads the marker immediately before and after a functional probe.
A stable eligible capability creates control and responsibility evidence. The
first healthy observation is provisional; the configured consecutive healthy
streak confirms acquisition. A different capability begins a new claim.

The previous cycle's team, or tied teams with the most confirmed healthy
control, normally receives the configured opening cooldown. If blocking all
tied leaders would leave no challenger, RSCTF omits the block. A blocked tick
is removed from that team's personal opportunity denominator.

### 2.2 Leaderboard protocol

The player-facing application receives a capability through its documented
mechanic, hashes it immediately with SHA-256, and records only its lowercase
digest beside independently verified events. A trusted referee runs outside
the player-controlled runtime.

For every active scoring round, the referee fetches a context bound to the
exact runtime, objective schema, and eligible capability hashes. It filters its
untrusted feed against that set and submits:

- one activity `earned/possible` ratio;
- one to sixteen objective `earned/possible` ratios; and
- an ordered `objectiveIds` list defining those positions.

The signed body contains `tokenHash`, not the bearer token. It contains no team
ID, floating-point score, integrity multiplier, or point value. RSCTF resolves
hashes against current capabilities and reports submitted and recognized row
counts.

The first accepted snapshot freezes the exact objective IDs and order. The
schema survives credential rotation. A later reorder, rename, insertion, or
deletion is rejected even when the component count stays the same.

The checker allows a bounded six-second arrival window, reads the complete
snapshot, performs an independent probe, and reads it again. Only a
byte-equivalent current snapshot plus a healthy verdict produces one dense
result row per frozen eligible team. Omitted teams receive zero. A changing
snapshot or unhealthy shared application voids the field-wide tick.

Leaderboard does not elect a holder, create a provisional crown, or apply
champion-cooldown scoring. Multiple teams can score in one tick.

### 2.3 Crown-cycle lifecycle

Both formats use the same durable sequence:

```text
finalize → pause → audit/snapshot → destroy
         → create → issue → readiness → resume
```

RSCTF stores every transition phase, prevents a second replica from owning the
same operation, launches one replacement from the official image, revokes old
capabilities, issues the new window, and resumes only after readiness succeeds.
A retry continues the stored phase instead of starting a parallel replacement.
Reset and readiness time create no scoring opportunity.

<figure class="journal-figure">
  <img src="/diagrams/koth-scoring-pipeline.svg" alt="KotH evidence pipeline from capability-scoped gameplay through checking, immutable tick evidence, epoch scoring, and settlement">
  <figcaption><strong>Figure 2.</strong> The checker admits current evidence; only immutable tick rows enter epoch scoring.</figcaption>
</figure>

### 2.4 Exact attribution and voids

At most one immutable checker result exists per game, hill, and scoring round.
Scorable evidence must match the frozen hill, active cycle, reset attempt,
container, deadline, capability window, and—for Leaderboard—the objective
schema.

`InternalError`, incomplete capability issuance, and uncertain runtime
inspection are platform voids. For Boot2Root, a responsible holder may receive
the last authoritative availability verdict before recovery. For Leaderboard,
a shared failure is field-wide void because it cannot be fairly assigned to
one participant.

## 3. Boot2Root scoring {#boot2root-scoring}

For team `i`, hill `h`, and epoch `e`, define:

<p class="journal-table-caption koth-keep-table"><strong>Table 2.</strong> Boot2Root evidence counters.</p>

| Symbol | Evidence |
| --- | --- |
| $x$ | crown-cycle windows in which the team confirmed acquisition |
| $y$ | windows in which the team had an eligible opportunity |
| $u$ | scorable ticks controlled by the exact capability |
| $s$ | personally eligible ticks after void/cooldown removal |
| $b$ | responsible ticks with checker verdict `Ok` |
| $d$ | ticks for which the team was responsible |

The three rates are:

$$A=x/y,\qquad C=u/s,\qquad R=b/d.$$

An empty denominator returns zero. The acquisition/control core is:

$$B^{M}=0.25A+0.55C+0.20\sqrt{AC}.$$

The local hill score is:

$$H^{M}=100RB^{M}.$$

Control has the largest direct coefficient. The geometric term rewards a team
that both takes and retains the hill; reliability constrains the entire score.

### 3.1 Worked Boot2Root example

Suppose a team confirms one of two eligible windows, controls three of eight
eligible ticks, and stays healthy in four of five responsible ticks:

$$A=0.5,\qquad C=0.375,\qquad R=0.8.$$

$$B^{M}=0.25(0.5)+0.55(0.375)+0.20\sqrt{0.5(0.375)}.$$

Thus `B^M = 0.41785` and the local score is `33.43`. Improving control raises
the main term and the acquisition/control balance term; breaking the service
reduces every earned control point through `R`.

## 4. Leaderboard scoring {#leaderboard-scoring}

### 4.1 Independent normalization

For team `i`, hill `h`, tick `t`, activity evidence is
`activityEarned/activityPossible`. Objective `j` is
`objectiveEarned/objectivePossible`. Every budget is an integer satisfying:

$$0\leq\mathrm{earned}\leq\mathrm{possible}\leq10^{12}.$$

RSCTF calculates:

$$E_t=\frac{a_t^+}{a_t^*}.$$

$$p_{tj}=\frac{o_{tj}^+}{o_{tj}^*},\qquad
P_t=\frac{1}{m}\sum_{j=1}^{m}p_{tj}.$$

Each native objective is first converted to a fixed integer normalization
scale. A `9/10` result and a `900/1000` result both contribute `0.9`; a larger
unit cannot become an accidental weight. Every objective has equal influence
unless the challenge publishes a single combined objective by design.

### 4.2 Per-tick performance

If either required channel is zero, then `Q_t = 0`. Otherwise:

$$Q_t=\frac{1}{0.35/E_t+0.65/P_t}.$$

This weighted harmonic mean makes both actual play and objective performance
necessary. The objective channel has greater influence, while either weak
channel remains a bottleneck. RSCTF stores `Q_t` before epoch aggregation, so
activity in one tick cannot combine with performance in another.

Failed hacking attempts are not subtracted. Invalid, forged, replayed, stale,
or wrongly scoped evidence is rejected or excluded as telemetry. The scored
mechanic must still require meaningful verified work; Section 7 defines that
challenge-design obligation.

### 4.3 Exact lead credit

For an epoch with `T` field-scorable ticks:

$$Q=\frac{1}{T}\sum_{t=1}^{T}Q_t.$$

A tick is competitive only when at least two teams have positive `Q_t`. If `k`
teams share the exact highest positive value, each receives `l_t = 1/k`. Every
other team receives zero. This avoids awarding a solo-field bonus and avoids a
team-ID tie-break.

Lead coverage is:

$$L=\frac{1}{T}\sum_{t=1}^{T}l_t.$$

For `T < 2`, sustained lead `S` is zero. Otherwise:

$$S=\frac{1}{T-1}\sum_{t=2}^{T}\min(l_{t-1},l_t).$$

The minimum preserves fractional tied-leader credit. `L` rewards reaching
first; `S` distinguishes consecutive leadership from scattered peaks.

### 4.4 Bounded sustained-first bonus

The dominance rate mirrors the Boot2Root acquisition/control shape:

$$D=0.25L+0.55S+0.20\sqrt{LS}.$$

The local Leaderboard score is:

$$H^{L}=100\left[Q+0.50Q(1-Q)D\right].$$

The first term pays for absolute challenge performance. The second rewards
sustained first place. It contributes nothing when `Q=0`, cannot push perfect
performance above 100, and is bounded by 12.5 points because
`Q(1-Q) <= 0.25` and `D <= 1`.

<p class="journal-table-caption koth-keep-table"><strong>Table 3.</strong> Equal absolute performance with different first-place patterns.</p>

| Ten-tick pattern at `Q = 0.80` | `L` | `S` | `D` | Score |
| --- | ---: | ---: | ---: | ---: |
| Never first | 0.000 | 0.000 | 0.000 | 80.00 |
| First on five alternating ticks | 0.500 | 0.000 | 0.125 | 81.00 |
| First for five consecutive ticks | 0.500 | 0.444 | 0.464 | 83.71 |
| First for all ten ticks | 1.000 | 1.000 | 1.000 | 88.00 |

The alternating and consecutive teams reach first equally often, but only the
second retains it across adjacent ticks. At `Q=0.20` and `D=1`, the result is
28 rather than a winner jackpot:

$$100[0.20+0.50(0.20)(0.80)]=28.$$

### 4.5 Same-tick rule

Consider two ticks. In tick one a team has activity `1.0` but objective
performance `0`; in tick two it has activity `0` and objective performance
`1.0`. Both `Q_t` values are zero, so the epoch performance is zero. Computing
the harmonic mean from the displayed epoch averages would incorrectly create
a positive score. RSCTF therefore persists and averages the per-tick core.

## 5. Hill aggregation, epochs, and rank {#aggregation}

Before scoring, each hill receives a frozen weight `w_h` in `[0.8,1.2]`. Let
`z_he = 1` when hill `h` has field-wide scorable evidence in epoch `e`, and
zero otherwise. The team epoch result is:

$$E_{ie}=\frac{\sum_h z_{he}w_hH_{ihe}}
                 {\sum_h z_{he}w_h}.$$

A wholly void hill contributes no numerator or denominator. Once a
Leaderboard hill has field evidence, an omitted team retains its explicit zero
rather than removing the hill from its personal calculation.

A complete epoch has weight `q_e = 1`. If the event ends after `r` of `n`
configured ticks in the final epoch, then `q_e = r/n`. The event score is:

$$T_i=\frac{\sum_e q_eE_{ie}}{\sum_e q_e}.$$

There is no late-epoch multiplier. Projected includes open epochs; Settled uses
only finalized epochs.

Official KotH rank sorts by:

1. Settled score descending;
2. Control for Boot2Root or Objective rate for Leaderboard;
3. Reliability for Boot2Root or Sustained-lead rate for Leaderboard;
4. acquisition-window count or activity-positive-tick count; and
5. stable participation ID.

The live projection never breaks an official tie. Displayed KotH ranks are
ordinal; the final ID provides deterministic ordering.

<p class="journal-table-caption koth-keep-table"><strong>Table 4.</strong> Source-aware scoreboard metrics.</p>

| Badge | First metric | Second metric | Third metric | Crown state |
| --- | --- | --- | --- | --- |
| Boot2Root | Acquisition | Control | Reliability | confirmed / pending |
| Leaderboard | Activity | Objective | Sustained lead | none |

## 6. Fault and admission policy {#fault-policy}

<p class="journal-table-caption"><strong>Table 5.</strong> Evidence admission and fault treatment.</p>

| Condition | Boot2Root | Leaderboard |
| --- | --- | --- |
| Missing own evidence in valid tick | no control/health credit | explicit zero row |
| Missing or changing whole snapshot | not applicable to marker feed | field-wide void |
| Functional checker fails globally | attributed only when responsibility is authoritative; otherwise void/recovery | field-wide void |
| Invalid/revoked capability | no claim | row rejected or unrecognized |
| Old cycle/reset/container | no claim | context rejected |
| Replay or old timestamp | not a marker operation | request rejected |
| Objective IDs/order changed | not applicable | request rejected |
| Reset/readiness/incomplete issuance | void | void |
| Failed player exploit attempt | no score by itself | no negative points; telemetry/rate limits still apply |

A `200` response from the signed endpoint only stages a snapshot. It does not
award points. The checker still has to bracket the snapshot around a healthy
probe and persist immutable evidence.

## 7. Security and anti-cheat contract {#security}

### 7.1 HMAC authenticates origin, not truth

Run the Leaderboard referee under a separate trusted identity and compute
boundary. Its HMAC secret must not be present in the player image, environment,
filesystem, browser, logs, repository, or challenge backup. A valid signature
shows that the referee produced an exact body; it does not show that the
referee measured honestly.

Give the referee read-only access to the smallest evidence feed it needs,
outbound access only to the arena and RSCTF, and persistent bounded cursor
state. Prevent the player-facing arena from calling the signed endpoint.
PostgreSQL backups contain the symmetric key and remain sensitive.

### 7.2 Make score require play

A defensible Leaderboard challenge:

1. counts completed challenge-relevant work, not requests, packets, page views,
   or connection volume;
2. issues unpredictable, expiring, one-use tasks or proofs;
3. binds each task and result to the capability hash that began it;
4. verifies results server-side and makes replay idempotent;
5. publishes fixed activity targets, objective IDs/order, and denominators;
6. bounds sessions, requests, evidence retention, pagination, and queue work;
7. keys team quotas by capability identity rather than source IP;
8. isolates per-team work so one team cannot exhaust shared admission;
9. exposes an ordered feed cursor and fails closed on a retention gap; and
10. keeps the functional checker independent and read-only.

Fuzzing, failed exploit development, and malformed challenge traffic are part
of hacking unless event rules prohibit a specific action. They do not deserve
an automatic score penalty. Rate limits and bounded work protect availability;
only verified completed outcomes create positive scoring evidence.

### 7.3 Capability and context scope

RSCTF resolves a submitted `tokenHash` only against a capability current for
the exact game, hill, target, cycle, reset attempt, and container. The context
also binds the round and frozen objective-schema hash. Raw bearer tokens never
enter the signed request. Unknown and stale hashes cannot become arbitrary
team identities.

The wire path remains `/api/v1/...` for compatibility. `v1`, `Marker`, and
`Api` are protocol or storage identifiers, not scoring-policy versions. The
database stores no selectable KotH formula column.

## 8. Organizer operations {#operations}

### 8.1 Preflight

Before official scoring:

- choose and publish each hill's format, cadence, bounded weight, checker
  contract, and appeal policy;
- confirm at least two accepted teams receive distinct current capabilities;
- run a pristine replacement and reject every previous capability afterward;
- verify the checker before and after evidence reads;
- for Boot2Root, verify provisional confirmation, takeover, responsibility,
  and enforceable cooldown;
- for Leaderboard, freeze the final objective IDs/order and verify HMAC,
  recognized/submitted equality, independent normalization, tied lead credit,
  explicit zero, referee restart, stale hash, replay, clock skew, feed gap, and
  changing-snapshot behavior; and
- run a complete accelerated epoch at expected peak capacity, including one
  deliberately overloaded team, while confirming that other teams and the
  referee retain reserved capacity.

The official snapshot records roster, hills, images, source, cadence, and
weights. Objective identity freezes on the first accepted Leaderboard
snapshot. These score-affecting inputs cannot change after the competition
boundary.

### 8.2 During play

Monitor checker gaps, void reasons, reset phases, runtime identity, snapshot
freshness, recognized-team counts, cursor lag, queue saturation, and referee
errors. Pause scoring before rotating a referee secret, submit fresh evidence,
verify it, then resume.

At event end, reject late evidence, complete authoritative in-flight checks
within the deadline fence, settle the proportional tail, and publish awards
from Settled results. Retain official configuration, capability issuance and
revocation, immutable checker rows, Leaderboard tick evidence, lifecycle
receipts, and epoch rollups through the appeal period.

### 8.3 Signed body summary

The complete wire contract and signing example are in the
[organizer referee guide](../organizers/koth-api-observer). The core body is:

```json
{
  "context": "<64 lowercase hex characters>",
  "objectiveIds": ["proof-strength", "solve-speed"],
  "teams": [
    {
      "tokenHash": "<sha256 of current capability>",
      "activity": {"earned": 4, "possible": 5},
      "objectives": [
        {"earned": 7, "possible": 10},
        {"earned": 750, "possible": 1000}
      ]
    }
  ]
}
```

## 9. Validation and limitations {#limitations}

The equations prove boundedness and zero conditions. Unit, database, and
lifecycle tests establish behavior for exercised cases. Neither proves that an
organizer chose competitively valid objectives.

Organizers should measure activity and objective distributions, lead
concentration, missing/void frequency, referee lag, feed-retention margin,
marker confirmation failures, control changes, repeat winners, reset latency,
and score sensitivity to targets and cadence.

A randomized checker sample cannot prove continuous state between observations.
Capability hashing cannot repair low-entropy tokens. HMAC cannot prove referee
correctness. Equal normalization cannot prove equal objective difficulty. The
formula cannot detect off-protocol collusion. A field-wide outage is void, so
weak resource isolation could let one participant manufacture a veto. These
remain challenge-design and operational responsibilities.

## 10. Conclusion {#conclusion}

RSCTF does not treat concurrent KotH as Boot2Root with a different transport.
Boot2Root rewards qualified acquisition, sustained exclusive control, and
service reliability. Leaderboard rewards absolute normalized performance and
adds a small, bounded premium for consistently remaining first.

Both formulas are constant and platform-owned. Dense zero rows prevent stale
team scores; field-wide voids prevent infrastructure failures becoming
participant penalties. Exact identity fences, independent checking, pristine
replacement, bounded hill normalization, and finalized epochs make results
reproducible. Fairness still depends on published rules, checker coverage,
objective design, referee isolation, network equality, and incident review.

## Appendix A. Frequently asked questions

### A.1 Is Leaderboard an observer for `/koth/king`?

No. Boot2Root has one holder and acquisition/control/reliability evidence.
Leaderboard has concurrent teams and activity/objective/sustained-lead
evidence. Only the lifecycle infrastructure is shared.

### A.2 Can the referee submit points or team IDs?

No. It submits integer evidence ratios and current capability hashes. RSCTF
resolves identity, normalizes evidence, detects leaders, and computes points.

### A.3 Why a harmonic mean?

It returns zero when either required play channel is zero and remains sensitive
to a weak channel. The fixed 35/65 weights give objective performance greater
influence without allowing it to replace activity.

### A.4 Why normalize objectives separately?

Raw sums let the largest numeric unit dominate. Independent ratios give every
objective the same `[0,1]` range before the equal mean.

### A.5 What if my team is omitted from a valid snapshot?

RSCTF writes an explicit zero for that tick. It never repeats an old result.

### A.6 What if the whole snapshot or application is unavailable?

The tick is field-wide void and enters no team's denominator.

### A.7 Do failed hacking attempts lose points?

No. Only meaningful verified outcomes add evidence. Invalid traffic can be
rate-limited and audited, but it is not an integrity score multiplier.

### A.8 Does a reset erase settled evidence?

No. It removes transient containers, sessions, claims, and capabilities.
Immutable results and finalized rollups remain.

### A.9 Is there formula versioning?

No. Boot2Root and Leaderboard are distinct formats with one constant formula
each. Protocol path `v1` is unrelated to scoring policy.

### A.10 Can automation participate?

Yes, subject to event rules. RSCTF validates evidence and scope rather than
guessing whether a participant is human or automated.

## Appendix B. Nomenclature

<p class="journal-table-caption"><strong>Table 6.</strong> KotH nomenclature.</p>

| Symbol or term | Definition |
| --- | --- |
| $A,C,R$ | Boot2Root acquisition, control, and reliability rates |
| $E_t,P_t$ | Leaderboard activity and normalized objective rates for tick `t` |
| $Q_t,Q$ | per-tick harmonic performance and its epoch mean |
| $l_t,L$ | tied-leader tick credit and epoch lead coverage |
| $S,D$ | sustained-lead continuity and bounded dominance rate |
| $H^M,H^L$ | local Boot2Root and Leaderboard scores in `[0,100]` |
| $w_h$ | frozen hill weight in `[0.8,1.2]` |
| $z_{he}$ | field-evidence switch for one hill and epoch |
| $q_e$ | complete or proportional final-epoch weight |
| **Provisional** | Boot2Root capability observed but not confirmed |
| **Settled** | official value using finalized epochs |
| **Projected** | information that also includes open evidence |
| **Field void** | sample excluded from every team's denominator |
| **Explicit zero** | omitted Leaderboard team in an otherwise valid tick |

## Appendix C. Implementation traceability {#implementation-traceability}

Paths are relative to the repository revision containing this paper.

<p class="journal-table-caption koth-traceability-table"><strong>Table 7.</strong> Implementation traceability.</p>

| Responsibility | Source of truth |
| --- | --- |
| Official source, roster, hill, cadence, image, and weight snapshot | `src/services/ad/engine/koth_cycle/config.rs` |
| Durable pristine replacement | `src/services/ad/engine/koth_cycle/lifecycle/` |
| Boot2Root claim and acquisition | `src/services/ad/engine/koth_cycle/claims.rs` |
| Exact marker read | `src/services/ad/engine/koth_marker.rs` |
| Signed context, HMAC, replay, schema, and submission | `src/controllers/game/koth/api/`, `api_contract.rs` |
| Stable snapshot read and tick core | `src/services/ad/engine/koth_api.rs` |
| Checker persistence, dense zeros, and tied leaders | `src/services/ad/engine/checker/koth_api.rs` |
| Constant pure formulas | `src/controllers/game/koth/scoring_formula.rs` |
| SQL epoch evidence and lead continuity | `src/controllers/game/koth/scoring/evidence.rs` |
| Final rollups | `src/controllers/game/koth/scoring/rollup/` |
| Board labels and rank | `src/controllers/game/koth/board.rs`, `web/src/components/KothScoreboardTable.tsx` |
| Constant Leaderboard schema and removal of formula selectors | `src/migrations/m0085_constant_leaderboard_scoring.rs` |

### C.1 HTTP surface

<p class="journal-table-caption"><strong>Table 8.</strong> Player, operator, and referee HTTP surface.</p>

| Method and route | Purpose |
| --- | --- |
| `GET /api/game/{id}/ad/koth/{challengeId}/token` | caller's exact current-hill capability |
| `GET /api/game/{id}/ad/koth/{challengeId}/state` | lifecycle and Boot2Root holder state |
| `GET /api/game/{id}/ad/koth/scoreboard` | source-aware metrics, Projected, Settled, rank |
| `GET /api/game/{id}/ad/koth/timeline` | finalized/projected cumulative history |
| `GET /api/edit/games/{id}/ad/koth/state` | operator lifecycle and evidence view |
| `POST /api/edit/games/{id}/ad/koth/{challengeId}/recover` | idempotent lifecycle recovery |
| `GET/POST/DELETE /api/edit/games/{id}/ad/koth/{challengeId}/observer` | inspect, create/rotate once, or revoke credential |
| `GET /api/v1/koth/games/{id}/challenges/{challengeId}/context` | exact runtime/schema fence and eligible hashes |
| `POST /api/v1/koth/games/{id}/challenges/{challengeId}/observations` | signed bounded evidence; never points |

Wire DTOs use camelCase and Unix-millisecond timestamps.

### C.2 Verification scope

The suite covers formula bounds and zero conditions, objective normalization,
ordered schema identity, malformed evidence, HMAC scope, clock skew, replay,
context rotation, current capability resolution, unknown hashes, dense zeros,
snapshot bracketing, tied lead credit, continuity, marker confirmation,
cooldown denominators, tied champions, partial epochs, rollups, and ordinal
ranks. The bundled challenge exercises one-use proofs, invalid traffic,
capability hashing, bounded pagination, persistent referee restart, feed-gap
failure, and exact signed bodies. Lifecycle and fixed-load results are recorded
in `tests/load/REPORT.md`; they do not establish competitive fairness.

## References

1. <span id="ref-1"></span>RSCTF Project, “King of the Hill implementation,” repository-local source artifact, constant Boot2Root and Leaderboard formulas, verified 2 August 2026.
2. <span id="ref-2"></span>K. Bock, G. Hughey, and D. Levin, “King of the Hill: A Novel Cybersecurity Competition for Teaching Penetration Testing,” in *Proceedings of the 2018 USENIX Workshop on Advances in Security Education*, Baltimore, MD, USA, 2018. [Online]. Available: [USENIX paper](https://www.usenix.org/conference/ase18/presentation/bock). Accessed: 28 July 2026.
3. <span id="ref-3"></span>CTFd, “King of the Hill,” *CTFd Documentation*, 2026. [Online]. Available: [CTFd KotH documentation](https://docs.ctfd.io/docs/custom-challenges/king-of-the-hill/). Accessed: 28 July 2026.
4. <span id="ref-4"></span>FAU Security Team, “Rules,” *FAUST CTF 2025*, 2025. [Online]. Available: [FAUST CTF rules](https://2025.faustctf.net/information/rules/). Accessed: 28 July 2026.
5. <span id="ref-5"></span>OtterSec, “Submit dynamic scores,” *rCTF Documentation*, 2026. [Online]. Available: [rCTF dynamic-score documentation](https://github.com/otter-sec/rctf/blob/main/apps/docs/src/content/docs/api/challenges/submit-dynamic-scores.md). Accessed: 28 July 2026.
