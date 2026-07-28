# Signed KotH API arena referee

RSCTF has two deliberately different King of the Hill formats:

- **Marker KotH** is an exclusive boot2root hill. One team controls the shared
  machine at a time, and RSCTF scores acquisition, control, and reliability.
- **API arena KotH** is a multi-team application challenge. Every team may
  produce evidence in the same scoring tick. RSCTF normalizes challenge-native
  measurements and scores activity, objective performance, and integrity.

The API is not a dynamic-points webhook. A trusted referee submits bounded
integer evidence, never team IDs, raw bearer capabilities, or points. This is
stricter than rCTF's signed dynamic-score API, which accepts externally
calculated per-user point values. RSCTF owns normalization, missing-team
behavior, the fixed formula, tick settlement, and the 0-100 ceiling. See
[rCTF's API contract](https://github.com/otter-sec/rctf/blob/main/apps/docs/src/content/docs/api/challenges/submit-dynamic-scores.md)
for the comparison.

## Fixed scoring rule

For one team in one scorable tick, the referee reports:

- activity evidence `earned / possible`;
- between one and sixteen objective evidence ratios; and
- integrity evidence `valid actions / total actions`.

RSCTF calculates:

```text
E = activity earned / activity possible
P = mean(each objective earned / each objective possible)
I = valid actions / total actions

B = 0                                      when E = 0 or P = 0
B = 1 / (0.35 / E + 0.65 / P)             otherwise

tick score = 100 * I * B
```

`B` is a weighted harmonic mean. Both activity and objective performance are
required, objective performance has the larger influence, and one excellent
channel cannot hide a weak channel. Integrity multiplies the same tick's
result. RSCTF persists that tick score before aggregating an epoch, so activity
from one tick cannot be combined with integrity or performance from another
tick to manufacture an outcome that never occurred.

Each native objective is normalized independently before the objective mean is
calculated. A challenge may therefore report, for example, `7/10` correctness
and `750/1000` throughput without the larger native scale dominating the
smaller one. The referee cannot configure weights or a scoring version.

An omitted eligible team receives explicit zero evidence for that tick. A
field-wide missing, changing, late, unhealthy, or incomplete snapshot voids the
tick instead of carrying an earlier result forward.

## Security boundary

Run the referee as an independent trusted service. Do not put its HMAC secret
in the player-facing arena image, environment, filesystem, browser, or logs. A
team may legitimately control or break challenge behavior; it must not acquire
the credential that speaks for the referee.

The arena should hash a submitted KotH capability immediately and retain only
its lowercase SHA-256 digest. The referee submits that digest as `tokenHash`.
RSCTF resolves it only against capabilities that are current for the exact
game, hill, cycle, reset attempt, target, and container. Raw bearer
capabilities never enter the signed evidence body.

The context response includes the current eligible capability hashes. The
referee must filter its untrusted arena feed against this set before building a
snapshot. This prevents fabricated token hashes from filling the bounded
submission. The hashes are identity filters, not bearer credentials.

Use HTTPS, synchronize the referee clock, store the secret under a dedicated OS
or orchestrator identity, restrict its outbound destinations, and rotate it
after suspected disclosure. PostgreSQL backups contain the symmetric referee
key and remain sensitive.

## Challenge-design requirements

The platform can validate evidence shape and identity, but the challenge
defines what an action proves. A fair API arena should follow these rules:

1. Count only verified, challenge-relevant actions as activity. Page views,
   polling, and unauthenticated requests are not play.
2. Use unpredictable, server-issued, expiring, one-use tasks or proofs. Bind
   each result to the capability hash that started it.
3. Publish fixed denominators and objective definitions before play. Do not
   choose a denominator after seeing team results.
4. Keep objectives conceptually distinct. Do not duplicate an objective entry
   to give it hidden weight; RSCTF intentionally gives each component equal
   normalized influence. Every team in a snapshot must use the same objective
   component count. RSCTF freezes that count on the first accepted snapshot
   containing a recognized team and rejects later changes. The frozen scheme
   belongs to the challenge and survives referee credential rotation or
   revocation. The meaning and order of those components must also remain
   fixed for the event.
5. Include failed attempts in integrity. Silently dropping failures makes
   guessing indistinguishable from correct play.
6. Make replay idempotent. A solved session, nonce, or receipt must not produce
   a second evidence event.
7. Bound request sizes, sessions, event retention, pagination, and rate-limit
   state. Apply per-capability, per-client, and global limits so arbitrary
   identities cannot grow memory without bound.
   Isolate per-capability work so one participant cannot exhaust shared
   capacity and manufacture a field-wide checker void.
8. Expose an ordered evidence cursor. If the referee detects a retention gap,
   it must fail closed and alert an operator rather than score a partial feed.
9. Keep the RSCTF functional checker independent and read-only. It verifies
   that the shared application works; it does not trust the scoring feed as a
   health check.
10. Rehearse at least one full epoch with honest clients, invalid attempts,
    referee restart, arena reset, stale capability, replay, and feed-gap
    scenarios before enabling official scoring.

The bundled
[`api-observed-hill`](https://github.com/dimasma0305/rsctf-challenges/tree/main/Koth/Web/api-observed-hill)
implements these properties with expiring one-use proof-of-work sessions,
bounded event pagination, persistent referee state, capability-hash filtering,
and two differently scaled objective channels.

## Enable an API arena

1. Open the game's **A&D / KotH operations** page.
2. Select **KotH**.
3. In the hill's **Claim input** column, choose **Enable API**.
4. Copy the one-time referee secret. RSCTF never returns its plaintext again.
5. Keep scoring paused and start the official KotH lifecycle.
6. Configure the referee with the game ID, challenge ID, RSCTF origin, stable
   arena URL, secret, and a persistent state-file path.
7. Run one preflight poll. Confirm that the operator view shows a current
   snapshot and that submitted and recognized team counts agree.
8. Exercise valid and invalid player actions, wait for checker evidence, then
   resume scoring.

The source is frozen in the official hill snapshot. A configured credential
selects `Api`; otherwise the hill uses `Marker`. It cannot change after scoring
starts. Rotating or revoking a live referee clears the current snapshot. Pause
scoring, rotate, submit fresh evidence, verify it, and resume.

API arenas support at most 2,000 accepted teams and 2,000 submitted team
entries. RSCTF rejects the official start when an enabled API arena exceeds
that roster bound.

## Wire contract

Fetch the current scoring fence:

```http
GET /api/v1/koth/games/{gameId}/challenges/{challengeId}/context
```

Example response:

```json
{
  "apiVersion": "v1",
  "context": "64-lowercase-hex-characters",
  "cycleNumber": 4,
  "resetAttempt": 0,
  "roundNumber": 17,
  "roundStartsAt": 1785123390000,
  "roundEndsAt": 1785123450000,
  "eligibleTokenHashes": [
    "ad4f...64-lowercase-hex-characters"
  ],
  "generatedAt": 1785123401000
}
```

`context` changes for every scoring round and every runtime/reset identity.
`eligibleTokenHashes` is the exact allowlist the referee should use for that
context.

Submit one complete current-tick snapshot:

```http
POST /api/v1/koth/games/{gameId}/challenges/{challengeId}/observations
Content-Type: application/json
X-RSCTF-Timestamp: 1785123402000
X-RSCTF-Signature: sha256=<lowercase-hex-HMAC-SHA256>

{
  "context": "<context>",
  "teams": [
    {
      "tokenHash": "<sha256-current-capability>",
      "activity": {"earned": 4, "possible": 5},
      "objectives": [
        {"earned": 7, "possible": 10},
        {"earned": 750, "possible": 1000}
      ],
      "integrity": {"earned": 19, "possible": 20}
    }
  ]
}
```

Send an empty `teams` array when nobody produced evidence. Do not omit the
snapshot; an explicit zero state lets operators distinguish no play from a
failed referee.

Every ratio must satisfy:

```text
0 <= earned <= possible <= 1,000,000,000,000
```

The body is limited to 512 KiB. `teams` is limited to 2,000 unique lowercase
64-character token hashes, and each team must contain one to sixteen objective
components. Every team in one snapshot must use the same component count.
Unknown or stale hashes are ignored and counted as submitted but not
recognized.

Compute the signature over the exact body bytes:

```text
HMAC-SHA256(
  key = referee secret UTF-8 bytes,
  message = timestamp + "." + gameId + "." + challengeId + "." + rawBody
)
```

The timestamp is Unix milliseconds and must be within five minutes of the
server. Accepted signatures are replay-protected for ten minutes. Within one
active context, a newly accepted timestamp must be greater than the stored
timestamp.

Example success:

```json
{
  "accepted": true,
  "cycleNumber": 4,
  "resetAttempt": 0,
  "roundNumber": 17,
  "submittedTeams": 2,
  "recognizedTeams": 2,
  "acceptedAt": 1785123402050
}
```

Require `recognizedTeams === submittedTeams` after filtering. A mismatch means
the capability window changed or the referee constructed evidence from an
invalid identity.

| Status | Meaning |
| --- | --- |
| `200` | The snapshot was staged for the checker; no score was awarded by the request itself. |
| `400` | JSON, evidence bounds, objective count, context shape, or hash shape is invalid. |
| `401` | Credential, timestamp window, or signature is invalid. These cases intentionally share one response. |
| `409` | Context changed, the snapshot is late/older, an accepted request was replayed, or the objective count differs from the event's frozen scheme. Fetch context and rebuild without changing the scheme. |
| `413` | The signed body exceeds 512 KiB. |
| `429` | The source exceeded the API rate limit. Back off. |

## Minimal signing example

This example assumes `teams` was built from independently verified,
current-round arena evidence:

```python
import hashlib
import hmac
import json
import os
import time
import urllib.request

ORIGIN = "https://ctf.example"
GAME_ID = 7
CHALLENGE_ID = 42
SECRET = os.environ["RSCTF_KOTH_OBSERVER_SECRET"]

base = f"{ORIGIN}/api/v1/koth/games/{GAME_ID}/challenges/{CHALLENGE_ID}"
with urllib.request.urlopen(f"{base}/context", timeout=5) as response:
    model = json.load(response)

eligible = set(model["eligibleTokenHashes"])
teams = [
    row for row in independently_verified_evidence()
    if row["tokenHash"] in eligible
]
body = json.dumps(
    {"context": model["context"], "teams": teams},
    separators=(",", ":"),
    sort_keys=True,
).encode()
timestamp = str(time.time_ns() // 1_000_000)
message = f"{timestamp}.{GAME_ID}.{CHALLENGE_ID}.".encode() + body
signature = hmac.new(SECRET.encode(), message, hashlib.sha256).hexdigest()

request = urllib.request.Request(
    f"{base}/observations",
    data=body,
    method="POST",
    headers={
        "Content-Type": "application/json",
        "X-RSCTF-Timestamp": timestamp,
        "X-RSCTF-Signature": f"sha256={signature}",
    },
)
with urllib.request.urlopen(request, timeout=5) as response:
    print(response.read().decode())
```

## What RSCTF does after submission

At the server-randomized checker time, RSCTF:

1. allows one bounded six-second arrival window for the exact current-round
   snapshot, reserving the complete checker and persistence budget;
2. runs the independent functional checker;
3. reads the snapshot again;
4. accepts only byte-equivalent, current evidence that bracketed the probe;
5. voids the field-wide tick if the shared application is unhealthy or the
   snapshot is missing/changing;
6. writes one immutable normalized row for every eligible team, including
   explicit zero rows for omitted teams;
7. calculates each team's harmonic-mean core and integrity-adjusted score for
   that tick; and
8. averages immutable tick results into projected and settled epochs.

The arena still receives pristine container replacements at crown-cycle
boundaries, which clears transient sessions and stale capabilities. API mode
does not elect a single holder, use provisional capture confirmation, or apply
champion cooldown scoring.

The arrival window accommodates the bundled referee's five-second default poll
without carrying evidence between rounds. A snapshot still has to match the
exact current round, cycle, reset attempt, target, and container; after six
seconds, absence remains a field-wide void.
