---
title: Boot2Root KotH and Leaderboard KotH
description: A two-format design proposal that separates exclusive machine control from concurrent normalized leaderboard competition.
pageClass: koth-handbook koth-format-paper
---

<div class="journal-title-block">
  <p class="journal-series">RSCTF SCORING DESIGN PAPER</p>
  <h1>Two Ways to Hold the Hill: Boot2Root KotH and Leaderboard KotH</h1>
  <p class="journal-authors">Dimas Maulana</p>
  <p class="journal-affiliation">RSCTF Project · Competition Platform</p>
  <p class="journal-correspondence">Format proposal · Version 1.0 · 30 July 2026</p>
  <p class="journal-policy">One fixed formula per format · no organizer-selected scoring versions</p>
</div>

<p class="pdf-download"><strong>Download:</strong> <a href="../downloads/king-of-the-hill-format-design.pdf" download>Two-format KotH design (A4 PDF)</a>.</p>

## Abstract

<div class="journal-abstract">
<p>This paper names and separates two King of the Hill competition formats. <strong>Boot2Root KotH</strong> is an exclusive-control contest over one shared machine: teams acquire the current hill, retain it, and remain responsible for service health. <strong>Leaderboard KotH</strong> is a concurrent application or protocol contest: every eligible team can play in the same tick, RSCTF normalizes challenge-native activity and objective evidence, and repeated first-place results earn a bounded sustained-lead bonus. The name “Leaderboard” describes the competitive object without restricting the challenge to HTTP APIs. The proposed formula never penalizes failed exploitation attempts as a score component. Evidence authenticity remains an admission decision enforced by scoped capabilities, signed referee snapshots, replay fencing, exact runtime context, and an independent functional checker. Both formats produce a bounded 0–100 hill score before common hill and epoch normalization.</p>
</div>

<p class="journal-keywords"><strong>Keywords:</strong> King of the Hill; Boot2Root; Leaderboard KotH; normalized objectives; sustained lead; cybersecurity competition; RSCTF</p>

<p class="journal-status"><strong>Status:</strong> design proposal. RSCTF <code>v0.1.31</code> calls the concurrent format “API arena” and still applies an integrity ratio to its tick score. This paper specifies the proposed replacement name and no-integrity-points formula. It is not the live scoring contract until the backend, UI, tests, and organizer documentation adopt it together.</p>

## 1. The format decision

RSCTF should expose two KotH types because they answer different competitive
questions:

- **Boot2Root KotH:** “Who controls this shared machine?”
- **Leaderboard KotH:** “Who performs best, and who can remain first?”

The first question has one observed holder. The second permits every team to
score concurrently. A transport label such as “API” does not express that
difference: a Leaderboard hill may be an HTTP service, binary protocol,
simulator, game server, cryptographic service, or another instrumented
application.

![Two KotH formats: exclusive Boot2Root control and concurrent Leaderboard competition](/diagrams/koth-two-format-model.svg)

<p class="journal-figure-caption"><strong>Figure 1.</strong> Boot2Root KotH measures exclusive machine control. Leaderboard KotH measures normalized concurrent performance and sustained first place. Both pass through the same bounded hill and epoch settlement.</p>

<p class="journal-table-caption"><strong>Table 1.</strong> Format contract.</p>

| Property | Boot2Root KotH | Leaderboard KotH |
| --- | --- | --- |
| Competitive object | One shared machine | One application or protocol objective set |
| Simultaneous scorers | At most one observed controller | Every eligible team |
| Primary player action | Exploit, claim, and retain the host | Complete meaningful actions and optimize objectives |
| Core evidence | Acquisition, control, reliability | Activity, objective performance, first-place continuity |
| Meaning of “king” | Current confirmed controller | Current normalized leaderboard leader |
| Failure experiments | Expected hacking behavior | Expected hacking behavior |
| Evidence fraud | Rejected or adjudicated | Rejected or adjudicated |
| Local score | `0–100` | `0–100` |

## 2. Why “Leaderboard KotH”

The proposed name has three useful properties.

First, it names what teams are trying to hold: first place on a challenge-local
leaderboard. Second, it remains accurate when the challenge is not an API.
Third, it gives the UI unambiguous language. A **Boot2Root** badge means that a
holder, provisional claim, confirmation, and takeover state exist. A
**Leaderboard** badge means that several teams can score in one tick and that
the board shows normalized performance plus sustained-lead evidence.

“Arena” remains a useful description, but it does not tell players what
constitutes control. “Objective KotH” describes the inputs but not the
first-place continuity requested by the format. “Leaderboard KotH” covers both.

## 3. Boot2Root KotH

### 3.1 Competition protocol

One managed hill exposes a machine or service that teams attack. RSCTF issues
each eligible team a high-entropy capability for the exact hill, crown cycle,
reset attempt, and container. A team claims the hill by placing its current
capability in the marker location after obtaining the required access.

The checker reads the marker before and after a functional service probe. A
stable eligible capability establishes control evidence. Confirmation requires
the configured healthy observation streak, so a transient write does not
become a capture. The controlling team remains responsible for service health
until another team takes over or the clean reset begins.

### 3.2 Fixed score

For one team, hill, and epoch:

- $A$ is the confirmed acquisition rate;
- $C$ is the fraction of personally eligible ticks controlled; and
- $R$ is service reliability while the team is responsible.

The constant Boot2Root score is:

$$
B^{\mathrm{root}}
=0.25A+0.55C+0.20\sqrt{AC},
$$

$$
H^{\mathrm{root}}=100R B^{\mathrm{root}}.
$$

Control has the largest direct coefficient. The geometric term rewards teams
that both acquire and retain the machine, while reliability constrains the
complete result. The formula does not change between events.

### 3.3 Meaning of consistent leadership

Consistency is physical and exclusive in Boot2Root KotH. A team earns it by
keeping the current capability on the machine across randomized healthy
checks. Another team cannot hold the same hill during those ticks.

Pristine crown-cycle replacement keeps that control contest repeatable. The
old container is destroyed, old capabilities are revoked, and the same frozen
image starts clean before the next cycle.

## 4. Leaderboard KotH

### 4.1 Competition protocol

Every eligible team uses the same player-facing application or protocol during
the same scoring tick. The challenge records capability-bound events. A trusted
referee outside the attacker-controlled runtime converts those events into
bounded integer ratios and signs one complete field snapshot. It submits
evidence, never points.

Players may probe, fuzz, crash their own session, send malformed inputs, and
fail while developing an exploit unless the published event rules prohibit a
specific action. Those attempts are part of hacking. RSCTF therefore does not
use “clean play” or a valid-attempt percentage as a score multiplier.

### 4.2 Native-score normalization

For team $i$, hill $h$, and tick $t$, let:

$$
E_{iht}=\frac{\text{meaningful activity completed}}
              {\text{published activity target}},
$$

and let objective $j$ report bounded integer evidence
$o^+_{ihtj}/o^\ast_{ihtj}$. RSCTF independently normalizes each objective:

$$
p_{ihtj}=\frac{o^+_{ihtj}}{o^\ast_{ihtj}},\qquad
P_{iht}=\frac{1}{m}\sum_{j=1}^{m}p_{ihtj},
\quad 1\leq m\leq16.
$$

A native result of `9/10` and another of `900/1000` both become `0.9`.
Numerical scale cannot become accidental weight. The challenge must publish
what earns each numerator and how each denominator is fixed.

Activity counts completed, capability-bound actions rather than request
volume. Suitable units include distinct tasks completed, protocol phases
reached, valid transactions committed, unique puzzles solved, or verified
work units. Raw HTTP requests, packets, or connection counts are unsuitable
because spam would become a scoring strategy.

### 4.3 Per-tick performance

The fixed performance core is a weighted harmonic mean:

$$
B_{iht}=
\begin{cases}
0, & E_{iht}=0\ \text{or}\ P_{iht}=0,\\[4pt]
\displaystyle
\frac{1}{0.35/E_{iht}+0.65/P_{iht}}, & \text{otherwise}.
\end{cases}
$$

Both participation and objective performance are necessary. Objective
performance has greater influence, but a team cannot replace activity with one
excellent output. RSCTF stores this result for each tick before aggregation,
so activity from one moment cannot combine with performance from another.

For $T$ field-scorable ticks in an epoch:

$$
Q_{ihe}=\frac{1}{T}\sum_{t=1}^{T}B_{iht}.
$$

$Q$ is the absolute challenge-performance rate. It does not depend on another
team's native point scale.

### 4.4 First-place and sustained-lead evidence

A competitive tick requires at least two teams with positive performance.
Within such a tick, the highest stored $B$ receives lead credit. If $k$ teams
share the exact highest result, each receives $1/k$ rather than an arbitrary
team-ID tie-break.

Let $\ell_{iht}$ be that lead credit. Lead coverage and sustained lead are:

$$
L_{ihe}=\frac{1}{T}\sum_{t=1}^{T}\ell_{iht},
$$

$$
S_{ihe}=
\begin{cases}
0, & T<2,\\[4pt]
\displaystyle
\frac{1}{T-1}\sum_{t=2}^{T}
\min(\ell_{ih,t-1},\ell_{iht}), & T\geq2.
\end{cases}
$$

$L$ rewards reaching first place. $S$ rewards remaining first across adjacent
ticks. Five scattered wins and five consecutive wins therefore produce
different consistency evidence.

The dominance rate deliberately mirrors the shape of the Boot2Root
acquisition/control core:

$$
D_{ihe}=0.25L_{ihe}+0.55S_{ihe}
       +0.20\sqrt{L_{ihe}S_{ihe}}.
$$

### 4.5 Bounded local score

The proposed Leaderboard hill score is:

$$
H^{\mathrm{lead}}_{ihe}
=100\left[
Q_{ihe}
+0.5Q_{ihe}(1-Q_{ihe})D_{ihe}
\right].
$$

The first term pays for absolute performance. The second is a bounded bonus
for sustained first place. It cannot create points when $Q=0$, cannot push a
perfect result above 100, and never raises the score beyond the `[0,100]`
range. Its largest possible contribution is 12.5 points, reached at $Q=0.5$
and $D=1$.

This structure avoids two common errors. A fixed ten-point winner award could
make weak performance valuable in an empty field. Multiplying by an uncapped
rank bonus could collapse several strong teams at the 100-point ceiling.
The bounded formula scales the bonus by both achieved performance and remaining
headroom.

## 5. Worked consistency example

Assume four teams each finish an epoch with $Q=0.80$ but reach first place in
different patterns over ten ticks.

<p class="journal-table-caption koth-keep-table"><strong>Table 2.</strong> Sustained leadership changes the bonus, not the underlying performance.</p>

| Pattern | $L$ | $S$ | $D$ | Final score |
| --- | ---: | ---: | ---: | ---: |
| Never first | 0.000 | 0.000 | 0.000 | 80.00 |
| First on five alternating ticks | 0.500 | 0.000 | 0.125 | 81.00 |
| First for five consecutive ticks | 0.500 | 0.444 | 0.464 | 83.71 |
| First for all ten ticks | 1.000 | 1.000 | 1.000 | 88.00 |

The alternating team reaches first as often as the five-tick leader, but it
never retains first place across adjacent ticks. The consecutive leader earns
the larger bonus. A team that remains first for the whole epoch receives the
maximum dominance rate.

The bonus does not replace challenge performance. At $Q=0.20$ and $D=1$, the
result is 28 rather than a winner jackpot:

$$
100[0.20+0.5(0.20)(0.80)(1)]=28.
$$

## 6. Hard to cheat, free to hack

The scoring boundary must separate exploratory behavior from forged evidence.

### 6.1 Hacking behavior

These actions do not directly reduce points:

- failed exploit attempts;
- fuzzing and malformed challenge inputs;
- restarting or corrupting a team-owned session;
- automation permitted by the event rules; and
- trying an objective without completing it.

An unsuccessful attempt simply produces no completed activity or objective
credit. Organizers may still rate-limit traffic to protect shared
infrastructure, but that is an operational control rather than a scoring
metric.

### 6.2 Evidence admission

These conditions reject or void evidence instead of applying a percentage
penalty:

- invalid HMAC or revoked referee credential;
- replayed body or non-monotonic timestamp;
- stale capability hash;
- wrong game, challenge, target, round, cycle, reset, or container;
- objective count that differs from the frozen challenge scheme;
- incomplete field snapshot;
- snapshot change during the independent functional probe; and
- platform-wide checker or readiness failure.

One omitted team inside an otherwise complete field snapshot receives an
explicit zero. A missing or untrustworthy whole-field snapshot is void for
everyone. Neither case carries an earlier score forward.

### 6.3 Rule violations

Collusion, attacking platform infrastructure, targeting organizer systems, or
other prohibited conduct belongs in the event's incident and sanction process.
A hidden fractional multiplier is too ambiguous for these decisions. The
organizer should preserve evidence, notify affected teams, and apply the
published warning, score correction, or disqualification procedure.

## 7. Shared settlement

Both formats produce one local hill score in `[0,100]`. RSCTF then applies the
same bounded hill and epoch aggregation.

Let $w_h\in[0.8,1.2]$ be the frozen hill weight and $z_{he}$ indicate whether
hill $h$ has field-scorable evidence in epoch $e$:

$$
E_{ie}
=\frac{\sum_h z_{he}w_hH_{ihe}}
       {\sum_h z_{he}w_h}.
$$

A wholly void hill contributes to neither numerator nor denominator. A
complete evidence-bearing epoch has weight one. A shortened final epoch uses
the fraction of configured ticks that were played:

$$
T_i=\frac{\sum_e q_eE_{ie}}{\sum_e q_e}.
$$

There is no late-event multiplier. **Projected** may include open evidence;
**Settled** includes finalized epochs and determines official rank.

## 8. Organizer selection guide

Choose **Boot2Root KotH** when all of the following are true:

- the intended skill is obtaining privileged access to one shared target;
- exactly one team should control the target at a time;
- takeover and persistence are central to the challenge;
- the service checker can attribute health responsibility to the holder; and
- clean resets preserve the intended exploit path.

Choose **Leaderboard KotH** when these conditions fit better:

- several teams should play and score simultaneously;
- the challenge exposes measurable application or protocol outcomes;
- at least one meaningful activity unit and one objective can be independently
  verified;
- first place and sustained first place should matter; and
- a trusted referee can operate outside the player-controlled runtime.

Do not choose Leaderboard KotH merely because a challenge has an HTTP API.
Use it when concurrent normalized competition is the intended game.

## 9. Proposed migration from API arena

The name and formula should change atomically in a future implementation:

1. rename the player-facing format from **API arena** to
   **Leaderboard KotH**, migrate the stored source identifier, and remove the
   superseded naming and scoring branch;
2. remove integrity from scoring and keep authenticity as an evidence gate;
3. add immutable per-tick lead credit and adjacent-tick sustained-lead
   evidence;
4. compute dominance and the final score only from finalized,
   field-scorable ticks;
5. update board columns to **Activity**, **Objectives**, and
   **Sustained lead**;
6. preserve the existing Boot2Root formula without scoring versions; and
7. migrate or void incompatible open API-arena evidence rather than
   reinterpreting it under the new policy.

The upgrade must run while no affected event is scoring. Settled numeric
rollups remain immutable historical results; RSCTF does not recompute them and
does not retain the superseded formula as a selectable scoring version.

## 10. Player summary

```text
BOOT2ROOT KOTH
Exploit the machine → claim it → keep it healthy → retain control

LEADERBOARD KOTH
Complete real actions → optimize every objective → reach first → stay first

EVIDENCE SECURITY
Authentic current evidence is accepted; forged or stale evidence is rejected
```

The two formats share the KotH idea but not the meaning of control. Boot2Root
control is possession of a shared machine. Leaderboard control is sustained
first place in a normalized concurrent challenge.

<div class="journal-break-page"></div>

## Appendix A. Symbols

<p class="journal-table-caption koth-keep-table"><strong>Table 3.</strong> Symbols used in the two-format scoring proposal.</p>

| Symbol | Definition |
| --- | --- |
| $A$ | Boot2Root confirmed acquisition rate |
| $C$ | Boot2Root controlled-tick rate |
| $R$ | Boot2Root service reliability while responsible |
| $E$ | Leaderboard meaningful activity rate for one tick |
| $P$ | Equal-weight mean of normalized objective ratios |
| $B$ | Leaderboard per-tick harmonic performance |
| $Q$ | Mean per-tick performance in one hill-epoch |
| $\ell$ | Exact first-place credit for one competitive tick |
| $L$ | First-place coverage |
| $S$ | Adjacent-tick sustained-lead rate |
| $D$ | Combined Leaderboard dominance rate |
| $H$ | Local hill score in `[0,100]` |

## References

1. RSCTF Project, “How RSCTF Scores King of the Hill: Crown Cycles and Normalized API Arenas,” implementation-aligned technical-practice report, version 3.0, 28 July 2026.
2. K. Bock, G. Hughey, and D. Levin, “King of the Hill: A Novel Cybersecurity Competition for Teaching Penetration Testing,” in *Proceedings of the 2018 USENIX Workshop on Advances in Security Education*, Baltimore, MD, USA, 2018. [Online]. Available: [USENIX paper page](https://www.usenix.org/conference/ase18/presentation/bock). Accessed: 30 July 2026.
3. OtterSec, “Submit dynamic scores,” *rCTF Documentation*, 2026. [Online]. Available: [rCTF dynamic-score documentation](https://github.com/otter-sec/rctf/blob/main/apps/docs/src/content/docs/api/challenges/submit-dynamic-scores.md). Accessed: 30 July 2026.
