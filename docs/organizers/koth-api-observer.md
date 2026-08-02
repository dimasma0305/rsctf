# Signed Leaderboard KotH referee

RSCTF has two constant King of the Hill scoring formats:

- **Boot2Root KotH** is exclusive control of a shared machine. One team is the
  confirmed holder at a time.
- **Leaderboard KotH** is a concurrent application or protocol challenge.
  Every eligible team can produce evidence in the same scoring tick.

Leaderboard KotH is not a dynamic-points webhook. A trusted referee submits
bounded integer evidence, never team IDs, bearer capabilities, or points.
RSCTF owns normalization, zero treatment, the fixed formula, lead detection,
tick settlement, and the 0–100 ceiling. There is no organizer-selectable
formula version.

## Constant scoring rule

For one team in one scorable tick, the referee reports:

- activity evidence `earned / possible`; and
- one to sixteen named objective evidence ratios.

RSCTF normalizes every objective independently, then calculates:

```text
E_t = activity earned / activity possible
P_t = mean(each objective earned / each objective possible)

Q_t = 0                                      when E_t = 0 or P_t = 0
Q_t = 1 / (0.35 / E_t + 0.65 / P_t)         otherwise
```

For an epoch containing `T` immutable scorable ticks:

```text
Q = mean(Q_t)

l_t = 1 / k   when this team is tied for the highest positive Q_t
              with k teams and at least two teams have positive Q_t
l_t = 0       otherwise

L = mean(l_t)
S = 0                                      when T < 2
S = sum(min(l_(t-1), l_t)) / (T - 1)      otherwise

D = 0.25L + 0.55S + 0.20 * sqrt(L * S)
Local = 100 * [Q + 0.50 * Q * (1 - Q) * D]
```

`Q_t` is a weighted harmonic mean: both meaningful play and objective
performance are necessary, with objective performance receiving the larger
influence. `L` measures exact first-place coverage and splits an exact tie.
`S` measures adjacent-tick continuity, so repeatedly holding first place is
more valuable than isolated peaks. The bonus is zero when `Q` is zero or one
and can never exceed 12.5 points. The final local score remains in `[0, 100]`.

Failed exploit attempts are not negative points. They may still be logged,
rate-limited, or reviewed as security telemetry, but the challenge must make
the scored action require real play: unpredictable tasks, verified results,
one-use receipts, and bounded capability-scoped quotas.

Each objective is normalized before the mean. For example, `7/10` correctness
and `750/1000` throughput contribute `0.70` and `0.75`; the larger native scale
does not dominate. Objective IDs and their order are frozen by the first
accepted snapshot and cannot change during the event.

An omitted eligible team receives explicit zero evidence for that tick. A
field-wide missing, changing, late, unhealthy, or incomplete snapshot voids
the tick for everyone instead of carrying an earlier result forward.

## Trust and isolation boundary

Run the referee as an independent trusted service. HMAC authenticates that a
body came from that service and was not changed in transit; it does **not**
prove that the measurements are honest. The referee must therefore be outside
the player-controlled challenge workload and use a separate identity,
filesystem, process/container boundary, and secret store.

The recommended deployment has these properties:

1. The player-facing arena cannot read the HMAC secret or referee state.
2. The referee has read-only access to the smallest evidence feed it needs and
   outbound access only to the arena and RSCTF.
3. The arena cannot call the signed RSCTF endpoint directly.
4. A team controls only work keyed by its KotH capability hash; it cannot spend
   another team's quota or an unbounded shared admission budget.
5. The independent RSCTF functional checker is read-only and does not accept
   the scoring feed as proof of health.

Do not put the HMAC secret in the player image, player-visible environment,
browser, logs, repository, or challenge backup. PostgreSQL backups contain the
symmetric referee key and remain sensitive. Use HTTPS, synchronize the clock,
restrict service identities and networks, and rotate the key after suspected
disclosure.

The arena should hash a submitted KotH capability immediately and retain only
its lowercase SHA-256 digest. The referee submits that digest as `tokenHash`.
RSCTF resolves it only against capabilities current for the exact game, hill,
cycle, reset attempt, target, and container. Raw capabilities never enter the
signed body.

The context response contains the current eligible hashes. Filter the
untrusted arena feed against this set before constructing the snapshot. These
hashes are identity filters, not bearer credentials.

## Challenge-design contract

A fair Leaderboard challenge must satisfy all of the following:

1. Count only verified, challenge-relevant actions as activity. Page views,
   polling, and unauthenticated traffic are not play.
2. Issue unpredictable, expiring, one-use tasks or proofs. Bind each result to
   the capability hash that began it.
3. Publish fixed activity targets, objective IDs, order, and meanings before
   play. Never choose a denominator after seeing results.
4. Keep objectives conceptually distinct. Duplicating an objective to create a
   hidden weight is prohibited; RSCTF gives every normalized objective equal
   influence.
5. Make replay idempotent. A completed session, nonce, or receipt cannot
   produce a second evidence event.
6. Bound request size, sessions, evidence retention, pagination, and
   rate-limit state. Key the team quota by capability identity, not source IP,
   so proxying or distributed addresses cannot consume extra team capacity.
7. Isolate per-team work and reserve referee/checker capacity. One participant
   must not be able to exhaust a global queue and manufacture field-wide voids.
8. Expose an ordered evidence cursor. A retention gap fails closed and alerts
   an operator; it never produces a partial score snapshot.
9. Keep the RSCTF functional checker independent of the referee and arena
   scoring database.
10. Rehearse at least one complete epoch at expected peak capacity, including
    valid play, invalid traffic, referee restart, arena reset, stale
    capabilities, replay, feed gaps, and one deliberately overloaded team.

The bundled
[`api-observed-hill`](https://github.com/dimasma0305/rsctf-challenges/tree/main/Koth/Web/api-observed-hill)
demonstrates expiring one-use proof sessions, bounded evidence pagination,
persistent referee state, capability-hash filtering, team-scoped admission,
and two differently scaled objective channels.

## Enable Leaderboard KotH

1. Open the game's **A&D / KotH operations** page and select **KotH**.
2. In the hill's **Claim input** column, choose **Enable Leaderboard**.
3. Copy the one-time referee secret. RSCTF never returns its plaintext again.
4. Keep scoring paused and start the official KotH lifecycle.
5. Configure the referee with the game ID, challenge ID, RSCTF origin, stable
   arena URL, secret, and persistent state path.
6. Fetch context and submit a preflight snapshot using the final ordered
   `objectiveIds`. The first accepted snapshot freezes that schema.
7. Fetch context again and confirm the returned IDs and schema hash match the
   referee configuration.
8. Exercise valid work, invalid traffic, omission, and restart behavior. Wait
   for checker evidence and verify submitted/recognized counts before resuming
   scoring.

The official hill snapshot stores the source. A configured credential selects
the internal `Api` claim source; otherwise the hill uses `Marker`. `Api` is a
stable wire/storage identifier, not a legacy formula selector. The source
cannot change after scoring starts.

Rotating or revoking a live credential clears the current snapshot. Pause
scoring, rotate, submit fresh evidence, verify it, and resume. Leaderboard
hills support at most 2,000 accepted teams and 2,000 submitted rows; official
start is rejected above that bound.

## Wire contract

Fetch the current scoring fence:

```http
GET /api/v1/koth/games/{gameId}/challenges/{challengeId}/context
```

Example response after the objective schema is frozen:

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
  "objectiveIds": ["proof-strength", "solve-speed"],
  "objectiveSchemaHash": "64-lowercase-hex-characters",
  "generatedAt": 1785123401000
}
```

Before the first accepted snapshot, `objectiveIds` is empty and
`objectiveSchemaHash` is `null`. The referee supplies the final schema in that
first submission. Thereafter the context is bound to its hash. `context`
changes with the scoring round, runtime/reset identity, or objective schema.

Submit one complete current-tick snapshot:

```http
POST /api/v1/koth/games/{gameId}/challenges/{challengeId}/observations
Content-Type: application/json
X-RSCTF-Timestamp: 1785123402000
X-RSCTF-Signature: sha256=<lowercase-hex-HMAC-SHA256>

{
  "context": "<context>",
  "objectiveIds": ["proof-strength", "solve-speed"],
  "teams": [
    {
      "tokenHash": "<sha256-current-capability>",
      "activity": {"earned": 4, "possible": 5},
      "objectives": [
        {"earned": 7, "possible": 10},
        {"earned": 750, "possible": 1000}
      ]
    }
  ]
}
```

The objective array positions must match `objectiveIds` exactly. Send an empty
`teams` array when nobody produced evidence, while retaining the same
`objectiveIds`. This explicit zero snapshot distinguishes no play from a
failed referee.

Every evidence ratio must satisfy:

```text
0 <= earned <= possible <= 1,000,000,000,000
```

The body is limited to 512 KiB. `teams` is limited to 2,000 unique lowercase
64-character token hashes. Objective IDs are unique, lowercase, 1–64 bytes,
begin with `a-z`, and otherwise contain only `a-z`, `0-9`, `-`, `_`, or `.`.
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

Require `recognizedTeams === submittedTeams` after local filtering. A mismatch
means the capability window changed or the referee used an invalid identity.

| Status | Meaning |
| --- | --- |
| `200` | Snapshot staged for the checker; this request itself awards no score. |
| `400` | JSON, evidence bounds, objective IDs/order, context, or hash shape is invalid. |
| `401` | Credential, timestamp window, or signature is invalid. These cases intentionally share one response. |
| `409` | Context changed, snapshot is late/older, replay was detected, or the objective schema differs from the frozen scheme. |
| `413` | Signed body exceeds 512 KiB. |
| `429` | Source exceeded the API rate limit; back off. |

## Minimal signing example

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
OBJECTIVE_IDS = ["proof-strength", "solve-speed"]
SECRET = os.environ["RSCTF_KOTH_OBSERVER_SECRET"]

base = f"{ORIGIN}/api/v1/koth/games/{GAME_ID}/challenges/{CHALLENGE_ID}"
with urllib.request.urlopen(f"{base}/context", timeout=5) as response:
    model = json.load(response)

if model["objectiveIds"] not in ([], OBJECTIVE_IDS):
    raise RuntimeError("RSCTF objective schema does not match this referee")

eligible = set(model["eligibleTokenHashes"])
teams = [
    row for row in independently_verified_evidence()
    if row["tokenHash"] in eligible
]
body = json.dumps(
    {
        "context": model["context"],
        "objectiveIds": OBJECTIVE_IDS,
        "teams": teams,
    },
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

At a server-randomized checker time, RSCTF:

1. allows one bounded six-second arrival window for the exact current-round
   snapshot;
2. runs the independent functional checker;
3. reads the snapshot again;
4. accepts only byte-equivalent current evidence bracketing that probe;
5. voids the field-wide tick if the shared application is unhealthy or the
   snapshot is missing or changing;
6. writes one immutable normalized row for every eligible team, including
   explicit zero rows for omitted teams;
7. calculates harmonic performance and tied lead credit for that tick; and
8. derives sustained-lead continuity and the constant score when projecting or
   settling the epoch.

The arena receives a pristine replacement at every crown-cycle boundary,
clearing transient sessions and stale capabilities. Leaderboard mode does not
elect a holder, use provisional capture confirmation, or apply champion
cooldown scoring. A snapshot must match the exact current round, cycle, reset
attempt, target, container, and objective schema; after the arrival window,
absence remains a field-wide void.
