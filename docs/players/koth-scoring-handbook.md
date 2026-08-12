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
  <p class="journal-correspondence">Unified edition · Version 6.2 · 12 August 2026</p>
  <p class="journal-policy">Scoring policy: one constant formula per KotH format; no scoring versions</p>
</div>

<p class="pdf-download"><strong>Download:</strong> <a href="../downloads/king-of-the-hill-scoring-handbook.pdf" download>King of the Hill handbook (A4 PDF)</a>.</p>

## Abstract

<div class="journal-abstract">
<p>RSCTF defines two King of the Hill (KotH) formats with different competitive meanings. <strong>Boot2Root KotH</strong> is an exclusive-control contest over one shared machine: teams acquire the hill, retain control, and accept responsibility for service health. <strong>Leaderboard KotH</strong> is a concurrent application or protocol contest: every eligible team may complete each challenge-native wave, and RSCTF compares every completed result with the best result from that same wave. Boot2Root uses the constant score 100R(0.25A + 0.55C + 0.20√AC). Leaderboard uses the constant wave score 100[0.95(S/S*)^(3/4) + 0.05K], where K is the unique Crown. An exact top-score tie gives every tied team full relative-performance credit and gives no team the Crown premium. Missing or incomplete teams receive zero for that wave. Points are not divided by field size, there is no separate winner or streak multiplier, and failed hacking attempts are not negative points. Authenticity, replay protection, exact runtime identity, independent checking, and challenge-level proof design determine whether evidence is admitted. Boot2Root capabilities rotate with scheduled pristine crown cycles. A Leaderboard arena remains persistent across rounds and epochs; its event token changes only through explicit security rotation, while a stopped runtime or repeated functional failure invokes health recovery. Both formats use scoped capabilities, bounded hill aggregation, and finalized epoch settlement. This paper is the canonical deployed scoring and operations contract.</p>
</div>

<p class="journal-keywords"><strong>Keywords:</strong> King of the Hill; Boot2Root; Leaderboard KotH; relative scoring; Crown; anti-cheat; crown cycle; RSCTF</p>

<p class="journal-status"><strong>Document status:</strong> current implementation contract for the repository revision containing this paper. Public names are Boot2Root and Leaderboard; the stable internal claim-source identifiers remain <code>Marker</code> and <code>Api</code>. Those identifiers are not selectable scoring versions. This is a technical-practice report, not a claim of peer review or empirical validation.</p>

<figure class="journal-figure">
  <img src="/diagrams/koth-two-format-model.svg" alt="Two King of the Hill formats: exclusive Boot2Root control and concurrent Leaderboard competition, both entering bounded epoch settlement">
  <figcaption><strong>Figure 1.</strong> Boot2Root measures exclusive machine control. Leaderboard measures fresh, relative concurrent performance and one recurring per-wave Crown.</figcaption>
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
2. finish a fresh, verified run in each wave;
3. maximize the published official result relative to the field; and
4. take or defend the unique Crown.

Players never call the organizer's signed referee endpoint.

### The two constant formulas

```text
BOOT2ROOT
Core  = 0.25A + 0.55C + 0.20 * sqrt(A * C)
Local = 100 * R * Core

LEADERBOARD
R_t   = 0 if incomplete; otherwise (S_t / S*_t)^(3/4)
K_t   = 1 for the unique Crown; otherwise 0
Wave  = 100 * (0.95R_t + 0.05K_t)
```

The first formula uses acquisition `A`, control `C`, and reliability `R`. The
second uses team result `S_t`, best result `S*_t`, relative performance `R_t`,
and Crown indicator `K_t` for each finalized wave.

### What resets and what settles

For **Boot2Root**, several ticks form a scheduled **crown cycle**. RSCTF then
pauses the hill, destroys the old container, creates a pristine replacement,
issues new cycle capabilities, proves readiness, and resumes scoring.

A **Leaderboard** arena has no scheduled Crown reset. It stays online across
rounds and epochs. RSCTF checks it every round and replaces it only when the
runtime stops or three consecutive challenge-attributed checks are unhealthy.
Recovery time is field-wide void, and each team's event capability survives.

Several ticks form an **epoch**. **Projected** includes unfinished evidence;
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
| Leaderboard | `Api` | every eligible team | completion, relative performance, Crown |

An API credential configured before official scoring selects internal source
`Api`; otherwise the hill uses `Marker`. The source cannot change after scoring
begins. “Leaderboard” describes the competition, not its transport: the player
surface may be HTTP, a binary protocol, a simulator, or a game server.

### 1.2 Shared invariants

Both formats preserve these requirements:

1. **Constant policy.** Each format has one formula and fixed coefficients.
2. **Bounded result.** A hill-epoch and aggregate epoch remain in `[0,100]`.
3. **Current identity.** Evidence belongs to the exact game, hill, round,
   lifecycle record, runtime attempt, target, and container. Boot2Root also
   binds the scheduled cycle capability; Leaderboard binds the current
   event-token generation.
4. **Independent health.** A functional checker, not the scoring feed, decides
   whether the shared application works.
5. **No stale carry.** A missing team in a valid Leaderboard snapshot becomes
   zero; a missing field snapshot is void. Neither repeats old evidence.
6. **Infrastructure neutrality.** Provisioning, recovery, readiness,
   incomplete issuance, and attributable platform failures do not penalize
   participants.
7. **Auditable settlement.** Immutable wave evidence precedes final rollups,
   and retries cannot create duplicate credit.
8. **Frozen bounded state.** Rosters, hills, cadence, images, weights, objective
   identities, request sizes, replay memory, and retained evidence are bounded.

### 1.3 Relation to other systems

Bock, Hughey, and Levin describe an instructional KotH in which taking a
shared machine also creates responsibility for critical services
[[2]](#ref-2). Boot2Root follows that exclusive-control interpretation while
adding scheduled pristine replacement and fixed epoch settlement.

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

Each team receives one opaque, high-entropy capability for the game and
Leaderboard hill. The player pastes only that value into the challenge. The
challenge scopes it to its configured game and hill, exchanges it with RSCTF,
and receives the authoritative team name plus a SHA-256 pseudonym. No local
crew ID or crew name is accepted in event mode. The challenge discards the raw
token and records only the lowercase pseudonym beside independently verified
events. A trusted referee runs outside the player-controlled runtime.

For every active scoring round, the referee fetches a context bound to the
exact runtime, objective schema, eligible capability hashes, and a published
settlement window. It filters its untrusted feed against that set and submits
zero or more finalized waves. Each wave contains:

- a stable wave ID and server-confirmed end time;
- one completed `activity` ratio and one to sixteen objective ratios per team;
- an ordered `objectiveIds` list defining those positions; and
- one `isCrown` assertion for a unique positive leader, otherwise none.

The signed body contains `tokenHash`, not the bearer token. It contains no team
ID, floating-point platform score, integrity multiplier, or point value. RSCTF
resolves hashes against current capabilities and reports finalized-wave plus
unique submitted/recognized-team counts.

Within one scoring context, every accepted finalized wave is append-only. A
later snapshot may add a new wave but cannot alter or remove an earlier one.

The capability survives health recovery and runtime replacement. A player may
explicitly rotate it after suspected exposure. Rotation immediately removes
the old digest from the eligible set and clears only that team's current
unsettled rows; every other team's evidence and already settled epoch evidence
remain immutable. The player must reconnect with the replacement token.

The first accepted snapshot freezes the exact objective IDs and order. The
schema survives credential rotation. A later reorder, rename, insertion, or
deletion is rejected even when the component count stays the same.

Settlement runs 20 seconds behind the live round boundary. The checker waits
for the published cutoff without holding a hill lock; the next round's window
starts at the previous cutoff, so finalized-wave end times are covered without
gaps. The checker then allows a bounded six-second arrival window, reads the
complete snapshot, performs an independent probe, and reads it again. Only a
byte-equivalent current snapshot plus a healthy verdict produces dense results
for every frozen eligible team across every finalized wave. Omitted teams
receive zero in that wave. A changing snapshot or unhealthy shared application
voids the field-wide checker tick.

The final published window end is the Leaderboard scoring cutoff. A challenge
must stop opening scoreable waves that cannot finish before it; the final
20-second reserve exists for the functional check and durable settlement, not
late gameplay.

Leaderboard names one challenge-native Crown when a positive finalized wave
has one unique leader. An exact top tie has no Crown. Leaderboard does not use
the Boot2Root marker, provisional confirmation, or champion cooldown. Multiple
teams can score in the same wave.

### 2.3 Source-aware runtime lifecycle

Boot2Root runs the durable sequence at every scheduled Crown boundary:

```text
finalize → pause → audit/snapshot → destroy
         → create → issue → readiness → resume
```

Leaderboard uses that state machine for initial provisioning, event cleanup,
and health recovery—not as a scoring clock. A stopped managed runtime starts
recovery immediately. Otherwise recovery requires three consecutive committed
`Mumble` or `Offline` functional verdicts for the same exact runtime and
attempt. `InternalError` does not count because uncertain platform inspection
must not destroy a healthy arena. Recovery recreates the frozen image, clears
transient snapshot/session state, preserves event capabilities, proves
readiness, and then resumes. A retry continues the stored phase instead of
starting a parallel replacement. Provisioning and recovery create no scoring
opportunity.

<figure class="journal-figure">
  <img src="/diagrams/koth-scoring-pipeline.svg" alt="Boot2Root KotH evidence pipeline from cycle-scoped gameplay through checking, immutable evidence, epoch scoring, and settlement">
  <figcaption><strong>Figure 2.</strong> The Boot2Root branch shown here admits only current evidence; Leaderboard follows the same immutable-evidence boundary with per-wave relative metrics.</figcaption>
</figure>

### 2.4 Exact attribution and voids

At most one immutable checker result exists per game, hill, and scoring round.
Scorable evidence must match the frozen hill, active lifecycle record, runtime
attempt, container, deadline, capability window, and—for Leaderboard—the
objective schema. For Boot2Root, that lifecycle record is the scheduled crown
cycle.

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

### 4.1 Fresh completion and native normalization

For team `i` and finalized wave `t`, activity is a completion gate. A scored
row must report a fresh server-confirmed completion for that wave. Partial
progress, polling, a previous result, or an omitted row produces zero.

For native objective `j`, every integer budget satisfies:

$$0\leq o^+_{itj}\leq o^*_{itj}\leq10^{12}.$$

RSCTF normalizes each objective independently and takes their equal mean:

$$O_{it}=\frac{1}{m}\sum_{j=1}^{m}\frac{o^+_{itj}}{o^*_{itj}}.$$

A `9/10` result and a `900/1000` result both contribute `0.9`; a larger unit
cannot become an accidental weight. A challenge such as Rythme may publish one
combined official score as its single objective. The referee still submits an
integer ratio rather than platform points.

### 4.2 Relative performance without field dilution

Let `O*_t` be the highest positive completed objective result in wave `t`.
Relative performance is zero without fresh completion. Otherwise:

$$R_{it}=\left(\frac{O_{it}}{O^*_t}\right)^{3/4}.$$

The three-quarter-power curve is fixed. It preserves order, maps the best
result to one, and keeps close competitors close. It does not divide a point
pool by the number of teams. Adding a new participant therefore cannot reduce
an existing team's points unless that participant posts a new best result.

### 4.3 One Crown only for a unique leader

`K_it` is one only for the wave's unique Crown and zero for every other team.
The Crown must have the highest positive completed result, and that result must
belong to exactly one team. If two or more teams tie for the highest result,
each receives full relative-performance credit and `K_it=0`; no timestamp,
incumbent state, roster order, or stable identity breaks the scoring tie.

The Crown and unique first place are the same award. This prevents an arbitrary
transport-order advantage while keeping the recurring five-point target for a
team that strictly leads the wave. Every Crown holder must complete fresh play
in that wave.

### 4.4 Constant wave score and epoch mean

The local result for one finalized wave is:

$$H^L_{it}=100\left(0.95R_{it}+0.05K_{it}\right).$$

Performance supplies at most 95 points and the recurring Crown supplies five.
There is no separate first-place bonus, participation bonus, exploit-status
multiplier, or growing streak multiplier. The official challenge result may
naturally include legal mechanics or an exploit-derived multiplier; RSCTF does
not convert a binary “exploited” label into points.

For `W` finalized waves in an epoch, RSCTF gives every wave equal influence:

$$H^L_i=\frac{1}{W}\sum_{t=1}^{W}H^L_{it}.$$

Several waves may finish inside one RSCTF checker round. Their signed summary
retains the wave count, so later epoch aggregation still weights each wave
equally rather than each transport submission equally.

<p class="journal-table-caption koth-keep-table"><strong>Table 3.</strong> One wave with native results 150, 20, 5, and 1.</p>

| Native result | Relative $R$ | Crown $K$ | Wave points |
| ---: | ---: | ---: | ---: |
| 150 | 1.0000 | 1 | 100.00 |
| 20 | 0.2207 | 0 | 20.96 |
| 5 | 0.0780 | 0 | 7.41 |
| 1 | 0.0233 | 0 | 2.22 |

If several teams score near 150, each receives nearly 95 performance points;
their scores do not collapse because the field is crowded. If only one team
completes with a positive result—even a native result of `1`—it is best for
that wave and receives 100. Teams tied at the best positive result each receive
95 because the wave has no unique Crown. Missing teams receive zero for the
wave.

Failed hacking attempts are not subtracted. Invalid, forged, replayed, stale,
or wrongly scoped evidence is rejected or excluded as telemetry. Section 7
defines the challenge-design obligations that make a completion meaningful.

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
3. Reliability for Boot2Root or Crown-share rate for Leaderboard;
4. acquisition-window count or completed-wave count; and
5. stable participation ID.

The live projection never breaks an official tie. Displayed KotH ranks are
ordinal; the final ID provides deterministic ordering.

<p class="journal-table-caption koth-keep-table"><strong>Table 4.</strong> Source-aware scoreboard metrics.</p>

| Badge | First metric | Second metric | Third metric | Crown state |
| --- | --- | --- | --- | --- |
| Boot2Root | Acquisition | Control | Reliability | confirmed / pending |
| Leaderboard | Completion | Relative objective | Crown share | per-wave Crown |

## 6. Fault and admission policy {#fault-policy}

<p class="journal-table-caption"><strong>Table 5.</strong> Evidence admission and fault treatment.</p>

| Condition | Boot2Root | Leaderboard |
| --- | --- | --- |
| Missing own evidence in valid tick | no control/health credit | explicit zero row |
| Missing or changing whole snapshot | not applicable to marker feed | field-wide void |
| Functional checker fails globally | attributed only when responsibility is authoritative; otherwise void/recovery | field-wide void |
| Invalid/revoked capability | no claim | row rejected or unrecognized |
| Old lifecycle/runtime/container | no claim | signed context rejected; event token remains usable after recovery |
| Replay, old timestamp, or future wave | not a marker operation | request rejected |
| Objective IDs/order changed | not applicable | request rejected |
| Provisioning/recovery/readiness/incomplete issuance | void | void |
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
5. publishes fixed completion rules, objective IDs/order, and denominators;
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

RSCTF resolves a submitted `tokenHash` only against the current event
capability for the exact game, hill, and official participation. The signed
context independently binds the round, target, lifecycle record, runtime
attempt, container, and frozen objective-schema hash. Compatibility fields
remain named `cycleNumber`, `resetAttempt`, and `cycleEndsAt`; for Leaderboard,
they fence runtime recovery and the event scoring cutoff rather than a
scheduled Crown reset. Raw bearer tokens enter only the narrowly scoped
authentication exchange and never enter the signed referee request. Unknown
and rotated hashes cannot become arbitrary team identities.

The wire path remains `/api/v1/...` for compatibility. `v1`, `Marker`, and
`Api` are protocol or storage identifiers, not scoring-policy versions. The
database stores no selectable KotH formula column.

## 8. Organizer operations {#operations}

### 8.1 Preflight

Before official scoring:

- choose and publish each hill's format, cadence, bounded weight, checker
  contract, and appeal policy;
- confirm at least two accepted teams receive distinct current capabilities;
- for Boot2Root, run a pristine replacement and reject every previous cycle
  capability afterward;
- verify the checker before and after evidence reads;
- for Boot2Root, verify provisional confirmation, takeover, responsibility,
  and enforceable cooldown;
- for Leaderboard, verify the arena stays on the same runtime through an
  ordinary Crown boundary, the same event token works after a forced health
  recovery, explicit rotation rejects the prior token and only that team's
  unsettled rows, and then freeze the final objective IDs/order and verify HMAC,
  submitted-wave and recognized/submitted equality, independent normalization,
  exact-tie no-Crown behavior, explicit zero, referee restart, stale hash, replay,
  clock skew, feed gap, and changing-snapshot behavior; and
- run a complete accelerated epoch at expected peak capacity, including one
  deliberately overloaded team, while confirming that other teams and the
  referee retain reserved capacity.

The official snapshot records roster, hills, images, source, cadence, and
weights. Objective identity freezes on the first accepted Leaderboard
snapshot. These score-affecting inputs cannot change after the competition
boundary.

### 8.2 During play

Monitor checker gaps, void reasons, Boot2Root reset phases, Leaderboard health
recovery, runtime identity, snapshot freshness, recognized-team counts, cursor
lag, queue saturation, and referee errors. Pause scoring before rotating a
referee secret, submit fresh evidence, verify it, then resume.

At event end, reject late evidence, complete authoritative in-flight checks
within the deadline fence, settle the proportional tail, and publish awards
from Settled results. Retain official configuration, capability issuance and
revocation, immutable checker rows, Leaderboard wave evidence, lifecycle
receipts, and epoch rollups through the appeal period.

### 8.3 Signed body summary

The complete wire contract and signing example are in the
[organizer referee guide](../organizers/koth-api-observer). The core body is:

```json
{
  "context": "<64 lowercase hex characters>",
  "objectiveIds": ["official-score"],
  "waves": [
    {
      "waveId": "heat-42",
      "endedAtUnixMs": 1786200000000,
      "teams": [
        {
          "tokenHash": "<sha256 of current capability>",
          "activity": {
            "earned": 1,
            "possible": 1
          },
          "objectives": [
            {
              "earned": 150,
              "possible": 150
            }
          ],
          "isCrown": true
        }
      ]
    }
  ]
}
```

## 9. Validation and limitations {#limitations}

The equations prove boundedness and zero conditions. Unit, database, and
lifecycle tests establish behavior for exercised cases. Neither proves that an
organizer chose competitively valid objectives.

Organizers should measure completion and objective distributions, Crown
concentration, missing/void frequency, referee lag, feed-retention margin,
marker confirmation failures, control changes, repeat winners, Boot2Root reset
latency, Leaderboard recovery frequency, and score sensitivity to targets and
cadence.

A randomized checker sample cannot prove continuous state between observations.
Capability hashing cannot repair low-entropy tokens. HMAC cannot prove referee
correctness. Equal normalization cannot prove equal objective difficulty. The
formula cannot detect off-protocol collusion. A field-wide outage is void, so
weak resource isolation could let one participant manufacture a veto. These
remain challenge-design and operational responsibilities.

## 10. Conclusion {#conclusion}

RSCTF does not treat concurrent KotH as Boot2Root with a different transport.
Boot2Root rewards qualified acquisition, sustained exclusive control, and
service reliability. Leaderboard rewards fresh performance relative to the
same wave's field and adds one recurring five-point Crown premium.

Both formulas are constant and platform-owned. Dense zero rows prevent stale
team scores; field-wide voids prevent infrastructure failures becoming
participant penalties. Exact identity fences, independent checking,
source-aware runtime lifecycle, bounded hill normalization, and finalized
epochs make results reproducible. Fairness still depends on published rules,
checker coverage, objective design, referee isolation, network equality, and
incident review.

## Appendix A. Frequently asked questions

### A.1 Is Leaderboard an observer for `/koth/king`?

No. Boot2Root has one holder and acquisition/control/reliability evidence.
Leaderboard has concurrent teams and completion/relative-performance/Crown
evidence. Only the lifecycle infrastructure is shared.

### A.2 Can the referee submit points or team IDs?

No. It submits integer evidence ratios, an optional per-wave Crown assertion, and
current capability hashes. RSCTF resolves identity, normalizes relative
performance, validates the Crown, and computes points.

### A.3 Why the three-quarter-power curve?

It preserves rank and maps the best completed result to one while keeping close
results competitive. It also avoids a shared point pool: field size alone
cannot dilute an existing score.

### A.4 Why normalize objectives separately?

Raw sums let the largest numeric unit dominate. Independent ratios give every
objective the same `[0,1]` range before the equal mean.

### A.5 What if my team is omitted from a valid snapshot?

RSCTF writes an explicit zero for that wave. It never repeats an old result.

### A.6 What if the whole snapshot or application is unavailable?

The checker tick is field-wide void and enters no team's denominator.

### A.7 Do failed hacking attempts lose points?

No. Only meaningful verified outcomes add evidence. Invalid traffic can be
rate-limited and audited, but it is not an integrity score multiplier.

### A.8 Does runtime replacement erase settled evidence?

No. A scheduled Boot2Root reset removes transient containers, sessions,
claims, and cycle capabilities. Leaderboard health recovery removes transient
runtime and snapshot state but preserves event tokens. Immutable results and
finalized rollups remain in both formats.

### A.9 Is there formula versioning?

No. Boot2Root and Leaderboard are distinct formats with one constant formula
each. Protocol path `v1` is unrelated to scoring policy.

### A.10 Can automation participate?

Yes, subject to event rules. RSCTF validates evidence and scope rather than
guessing whether a participant is human or automated.

<div class="journal-break-page"></div>

## Appendix B. Nomenclature

<div class="journal-table koth-keep-table koth-nomenclature-table">
<table>
<caption><strong>Table 6.</strong> KotH nomenclature.</caption>
<colgroup><col class="koth-term"><col><col class="koth-term"><col></colgroup>
<thead><tr><th>Symbol or term</th><th>Definition</th><th>Symbol or term</th><th>Definition</th></tr></thead>
<tbody>
<tr><td><i>A, C, R</i></td><td>Boot2Root acquisition, control, and reliability rates</td><td><i>z<sub>he</sub></i></td><td>field-evidence switch for one hill and epoch</td></tr>
<tr><td><i>S<sub>it</sub>, S<sup>*</sup><sub>t</sub></i></td><td>team and best completed native results in wave <code>t</code></td><td><i>q<sub>e</sub></i></td><td>complete or proportional final-epoch weight</td></tr>
<tr><td><i>R<sub>it</sub></i></td><td>three-quarter-power relative performance in wave <code>t</code></td><td><strong>Settled</strong></td><td>official value using finalized epochs</td></tr>
<tr><td><i>K<sub>it</sub></i></td><td>unique Crown indicator in wave <code>t</code></td><td><strong>Projected</strong></td><td>information that also includes open evidence</td></tr>
<tr><td><i>H<sup>M</sup>, H<sup>L</sup></i></td><td>local Boot2Root and Leaderboard scores in <code>[0,100]</code></td><td><strong>Field void</strong></td><td>sample excluded from every team's denominator</td></tr>
<tr><td><i>w<sub>h</sub></i></td><td>frozen hill weight in <code>[0.8,1.2]</code></td><td><strong>Explicit zero</strong></td><td>omitted or incomplete Leaderboard team in a valid wave</td></tr>
</tbody>
</table>
</div>

## Appendix C. Implementation traceability {#implementation-traceability}

Paths are relative to the repository revision containing this paper.

<p class="journal-table-caption koth-traceability-table"><strong>Table 7.</strong> Implementation traceability.</p>

| Responsibility | Source of truth |
| --- | --- |
| Official source, roster, hill, cadence, image, and weight snapshot | `src/services/ad/engine/koth_cycle/config.rs` |
| Source-aware scheduled reset and health-recovery lifecycle | `src/services/ad/engine/koth_cycle/lifecycle/` |
| Boot2Root claim and acquisition | `src/services/ad/engine/koth_cycle/claims.rs` |
| Exact marker read | `src/services/ad/engine/koth_marker.rs` |
| Leaderboard event-token authentication and rotation | `src/services/ad/koth_api_capability.rs`, `src/controllers/game/koth/tokens.rs` |
| Signed context, HMAC, replay, schema, and submission | `src/controllers/game/koth/api/`, `api_contract.rs` |
| Stable finalized-wave snapshot read and relative curve | `src/services/ad/engine/koth_api.rs` |
| Checker persistence, dense zeros, and Crown validation | `src/services/ad/engine/checker/koth_api.rs` |
| Constant pure formulas | `src/controllers/game/koth/scoring_formula.rs` |
| Equal-wave SQL epoch aggregation | `src/controllers/game/koth/scoring/evidence.rs` |
| Final rollups | `src/controllers/game/koth/scoring/rollup/` |
| Board labels and rank | `src/controllers/game/koth/board.rs`, `web/src/components/KothScoreboardTable.tsx` |
| Constant Leaderboard schema and removal of formula selectors | `src/migrations/m0085_constant_leaderboard_scoring.rs` |
| Event-scoped Leaderboard token schema and live-token preservation | `src/migrations/m0086_koth_api_event_tokens.rs` |
| Finalized-wave contract and constant 95/5 relative scoring | `src/migrations/m0088_koth_api_wave_scoring.rs` |

### C.1 HTTP surface

<p class="journal-table-caption"><strong>Table 8.</strong> Player, operator, and referee HTTP surface.</p>

| Method and route | Purpose |
| --- | --- |
| `GET /api/game/{id}/ad/koth/{challengeId}/token` | caller's exact current-hill capability |
| `POST /api/game/{id}/ad/koth/{challengeId}/token` | explicitly rotate a Leaderboard event capability |
| `POST /api/v1/koth/capability/authenticate` | exchange one scoped event capability for the authoritative arena identity |
| `GET /api/game/{id}/ad/koth/{challengeId}/state` | lifecycle and Boot2Root holder state |
| `GET /api/game/{id}/ad/koth/scoreboard` | source-aware metrics, Projected, Settled, rank |
| `GET /api/game/{id}/ad/koth/timeline` | finalized/projected cumulative history |
| `GET /api/edit/games/{id}/ad/koth/state` | operator lifecycle and evidence view |
| `POST /api/edit/games/{id}/ad/koth/{challengeId}/recover` | idempotent lifecycle recovery |
| `GET/POST/DELETE /api/edit/games/{id}/ad/koth/{challengeId}/observer` | inspect, create/rotate once, or revoke credential |
| `GET /api/v1/koth/games/{id}/challenges/{challengeId}/context` | exact runtime/schema fence, event cutoff, and eligible hashes |
| `POST /api/v1/koth/games/{id}/challenges/{challengeId}/observations` | signed bounded evidence; never points |

Wire DTOs use camelCase and Unix-millisecond timestamps.

### C.2 Verification scope

The suite covers formula bounds and zero conditions, objective normalization,
ordered schema identity, malformed evidence, HMAC scope, clock skew, replay,
context rotation, current capability resolution, unknown hashes, dense zeros,
snapshot bracketing, relative performance, exact-tie no-Crown handling, marker
confirmation, API persistence past a scheduled Crown boundary, three-failure
health recovery, cooldown denominators, tied champions, partial epochs,
rollups, and ordinal ranks. The bundled challenge exercises one-use proofs,
invalid traffic,
capability hashing, bounded pagination, persistent referee restart, feed-gap
failure, and exact signed bodies. Lifecycle and fixed-load results are recorded
in `tests/load/REPORT.md`; they do not establish competitive fairness.

## References

1. <span id="ref-1"></span>RSCTF Project, “King of the Hill implementation,” repository-local source artifact, constant Boot2Root and Leaderboard formulas, verified 2 August 2026.
2. <span id="ref-2"></span>K. Bock, G. Hughey, and D. Levin, “King of the Hill: A Novel Cybersecurity Competition for Teaching Penetration Testing,” in *Proceedings of the 2018 USENIX Workshop on Advances in Security Education*, Baltimore, MD, USA, 2018. [Online]. Available: [USENIX paper](https://www.usenix.org/conference/ase18/presentation/bock). Accessed: 28 July 2026.
3. <span id="ref-3"></span>CTFd, “King of the Hill,” *CTFd Documentation*, 2026. [Online]. Available: [CTFd KotH documentation](https://docs.ctfd.io/docs/custom-challenges/king-of-the-hill/). Accessed: 28 July 2026.
4. <span id="ref-4"></span>FAU Security Team, “Rules,” *FAUST CTF 2025*, 2025. [Online]. Available: [FAUST CTF rules](https://2025.faustctf.net/information/rules/). Accessed: 28 July 2026.
5. <span id="ref-5"></span>OtterSec, “Submit dynamic scores,” *rCTF Documentation*, 2026. [Online]. Available: [rCTF dynamic-score documentation](https://github.com/otter-sec/rctf/blob/main/apps/docs/src/content/docs/api/challenges/submit-dynamic-scores.md). Accessed: 28 July 2026.
