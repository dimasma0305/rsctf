# Signed Leaderboard KotH referee

RSCTF has two constant King of the Hill scoring formats:

- **Boot2Root KotH** is exclusive control of a shared machine. One team is the
  confirmed holder at a time.
- **Leaderboard KotH** is a concurrent application or protocol challenge.
  Every eligible team can produce evidence in the same finalized wave.

Leaderboard KotH is not a dynamic-points webhook. A trusted referee submits
bounded integer evidence, never team IDs, bearer capabilities, or points.
RSCTF owns normalization, zero treatment, the fixed formula, wave settlement,
and the 0–100 ceiling. There is no organizer-selectable
formula version.

## Constant scoring rule

For every challenge-native wave finalized inside the current RSCTF scoring
round, the referee reports:

- a stable wave ID and server-confirmed end time;
- completed activity evidence `1 / 1` for each submitted team;
- one to sixteen named objective evidence ratios; and
- at most one Crown assertion.

RSCTF normalizes every objective independently. Let `O_it` be team `i`'s mean
normalized objective result in wave `t`, and let `O*_t` be the highest positive
completed result in that wave. It then calculates:

```text
R_it = 0                               when completion is not exactly 1
R_it = (O_it / O*_t)^(3/4)             otherwise
K_it = 1 for the unique Crown, 0 otherwise
Wave_it = 100 * (0.95 * R_it + 0.05 * K_it)
```

The exponent is concave: close results remain close to the 95-point performance
ceiling, while weak results still separate. Points are not divided by the
number of players. The Crown is first place and contributes the remaining five
points in every wave. There is no separate winner award, shared-pool dilution,
or growing streak multiplier. The epoch result is the mean of immutable wave
results and remains in `[0, 100]`.

The referee must assert exactly one Crown when a wave has one unique positive
leader. The asserted team must have the best normalized result. On an exact
top tie, assert no Crown: every tied team receives full relative-performance
credit, but none receives the five-point premium. RSCTF rejects a Crown on a
tied wave, so transport timing and stable identity cannot become score
tie-breakers.

Failed exploit attempts are not negative points. They may still be logged,
rate-limited, or reviewed as security telemetry, but the challenge must make
the scored action require real play: unpredictable tasks, verified results,
one-use receipts, and bounded capability-scoped quotas.

Each objective is normalized before the mean. For example, `7/10` correctness
and `750/1000` throughput contribute `0.70` and `0.75`; the larger native scale
does not dominate. Objective IDs and their order are frozen by the first
accepted snapshot and cannot change during the event.

An omitted eligible team receives explicit zero evidence for that wave. A
signed wave with no team rows means nobody completed it and has no Crown. A
snapshot with no finalized waves means there was no competition opportunity in
that RSCTF round. A field-wide missing, changing, late, unhealthy, or incomplete
snapshot voids the checker tick instead of carrying an earlier result forward.

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

The arena should exchange a submitted KotH capability with RSCTF immediately,
then retain only the returned lowercase SHA-256 pseudonym. The referee submits
that value as `tokenHash`. RSCTF resolves it against the current event
capability for the exact game, hill, and official participation. The signed
context separately binds the lifecycle record, runtime attempt, target,
container, round, and objective schema. Raw capabilities never enter the
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
3. Publish the completion condition, objective IDs, order, and meanings before
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
    valid play, invalid traffic, referee restart, forced arena health recovery,
    stale capabilities, replay, feed gaps, and one deliberately overloaded
    team.

The bundled
[`api-observed-hill`](https://github.com/dimasma0305/rsctf-challenges/tree/main/Koth/Web/api-observed-hill)
demonstrates expiring one-use proof sessions, bounded evidence pagination,
persistent referee state, capability-hash filtering, team-scoped admission,
and two differently scaled objective channels.

## Enable Leaderboard KotH

1. Open the game's **A&D / KotH operations** page and select **KotH**.
2. In the hill's **Claim input** column, choose **Enable Leaderboard**.
3. Copy the one-time referee secret. If the mutation response is ambiguous,
   RSCTF recovers the exact result only for that same authorized operation;
   the operator UI performs that recovery before it permits another change.
4. Keep scoring paused and provision the official persistent arena lifecycle.
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
hills support at most 2,000 accepted teams, 64 waves per snapshot, and 2,000
total team-wave rows; official start is rejected above the roster bound.

Credential mutations carry an opaque `operationId` and the observer `revision`
shown to the operator. A retry with that same authorized operation recovers the
same result; a different operation against a stale revision is rejected. After
an ambiguous response, recover the known operation before issuing another
rotation or revocation.

## Wire contract

The player-facing arena authenticates one token without accepting a local team
ID or crew name:

```http
POST /api/v1/koth/capability/authenticate
Content-Type: application/json

{"token":"koth_…","gameId":203,"challengeId":995}
```

RSCTF returns `{"teamId":"<sha256-token>","teamName":"<official name>"}`.
The arena must discard the raw token after issuing its narrow local session.
The token remains valid across rounds, epochs, and health recovery; an explicit
player rotation invalidates the old value immediately and requires a new arena
session.

The organizer-controlled referee then fetches the current scoring fence:

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
  "cycleEndsAt": 1785123970000,
  "waveWindowStartsAt": 1785123370000,
  "waveWindowEndsAt": 1785123430000,
  "eligibleTokenHashes": [
    "ad4f...64-lowercase-hex-characters"
  ],
  "objectiveIds": ["official-score"],
  "objectiveSchemaHash": "64-lowercase-hex-characters",
  "generatedAt": 1785123401000
}
```

Before the first accepted snapshot, `objectiveIds` is empty and
`objectiveSchemaHash` is `null`. The referee supplies the final schema in that
first submission. Thereafter the context is bound to its hash. `context`
changes with the scoring round, runtime/recovery identity, objective schema, or
eligible capability set. A player token rotation therefore requires a fresh
context before the next submission. The compatibility field `cycleEndsAt` is
the event's Leaderboard scoring cutoff, not a scheduled reset boundary. A
challenge that runs multi-minute sessions must not start a new scoreable
session unless it can finish and report before that timestamp; reconnects to
an already-running session may continue.

Submit the complete set of finalized waves whose end times fall inside the
current context's settlement window:

```http
POST /api/v1/koth/games/{gameId}/challenges/{challengeId}/observations
Content-Type: application/json
X-RSCTF-Timestamp: 1785123402000
X-RSCTF-Signature: sha256=<lowercase-hex-HMAC-SHA256>

{
  "context": "<context>",
  "objectiveIds": ["official-score"],
  "waves": [
    {
      "waveId": "heat-42",
      "endedAtUnixMs": 1785123400000,
      "teams": [
        {
          "tokenHash": "<sha256-current-capability>",
          "activity": {"earned": 1, "possible": 1},
          "objectives": [
            {"earned": 150, "possible": 150}
          ],
          "isCrown": true
        }
      ]
    }
  ]
}
```

The objective array positions must match `objectiveIds` exactly. `waveId` is a
stable 1–128 byte identifier using ASCII letters, digits, `.`, `_`, `:`, or
`-`; it must begin with a letter or digit. `endedAtUnixMs` must fall inside the
context's `[waveWindowStartsAt, waveWindowEndsAt)` interval and cannot be later
than the RSCTF server's current time. Send an empty `teams` array
when nobody completed that finalized wave. Send an empty `waves` array when no
wave finalized in the settlement window. Retain the same `objectiveIds` in both
cases.

RSCTF closes each settlement window 20 seconds behind the live checker-round
boundary. The checker waits for that cutoff before it acquires the hill lock,
then allows the referee a bounded arrival period. From round two onward, the
next window starts at the previous cutoff, so the intervals are contiguous and
no challenge-native wave end can fall into a gap. Use the published window
fields exactly; do not derive them from the round number or local clock. The
last published window end is the event's Leaderboard scoring cutoff. The arena
must not open a new scoreable wave that cannot finalize by that cutoff; the
remaining 20 seconds are reserved for checking and durable settlement.

Every completed positive wave with one unique leader must contain exactly one
`isCrown: true` row for that leader. A tied or zero-result wave has no Crown.
Token hashes are unique within a wave; the same team may and
normally will appear in several waves.

Finalized waves are append-only within a context. Every later snapshot must
preserve the already accepted wave prefix exactly after capability resolution,
then may append newly finalized waves. Changing or removing an older wave is
rejected; a referee must fail closed rather than revise history.

Every evidence ratio must satisfy:

```text
0 <= earned <= possible <= 1,000,000,000,000
```

The body is limited to 512 KiB. `waves` is limited to 64 entries and the whole
snapshot to 2,000 team-wave rows. `tokenHash` is a lowercase 64-character
SHA-256 value. Objective IDs are unique, lowercase, 1–64 bytes, begin with
`a-z`, and otherwise contain only `a-z`, `0-9`, `-`, `_`, or `.`. Unknown or
stale hashes are ignored and counted as submitted but not recognized.

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
  "submittedWaves": 2,
  "submittedTeams": 2,
  "recognizedTeams": 2,
  "acceptedAt": 1785123402050
}
```

Require `submittedWaves` to equal the emitted wave count and
`recognizedTeams === submittedTeams` after local filtering. Team counts are
unique identities across all waves, not the number of team-wave rows. A
mismatch means the capability window changed or the referee used an invalid
identity.

| Status | Meaning |
| --- | --- |
| `200` | Snapshot staged for the checker; this request itself awards no score. |
| `400` | JSON, evidence bounds, objective IDs/order, context, or hash shape is invalid. |
| `401` | Credential, timestamp window, or signature is invalid. These cases intentionally share one response. |
| `409` | Context changed, snapshot is late/older, a finalized wave changed, replay was detected, or the objective schema differs from the frozen scheme. |
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
OBJECTIVE_IDS = ["official-score"]
SECRET = os.environ["RSCTF_KOTH_OBSERVER_SECRET"]

base = f"{ORIGIN}/api/v1/koth/games/{GAME_ID}/challenges/{CHALLENGE_ID}"
with urllib.request.urlopen(f"{base}/context", timeout=5) as response:
    model = json.load(response)

if model["objectiveIds"] not in ([], OBJECTIVE_IDS):
    raise RuntimeError("RSCTF objective schema does not match this referee")

eligible = set(model["eligibleTokenHashes"])
waves = []
for wave in finalized_waves(
    model["waveWindowStartsAt"], model["waveWindowEndsAt"]
):
    teams = [row for row in wave["teams"] if row["tokenHash"] in eligible]
    waves.append({
        "waveId": wave["id"],
        "endedAtUnixMs": wave["endedAtUnixMs"],
        "teams": teams,
    })
body = json.dumps(
    {
        "context": model["context"],
        "objectiveIds": OBJECTIVE_IDS,
        "waves": waves,
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
   snapshot containing zero or more finalized waves;
2. runs the independent functional checker;
3. reads the snapshot again;
4. accepts only byte-equivalent current evidence bracketing that probe;
5. voids the field-wide tick if the shared application is unhealthy or the
   snapshot is missing or changing;
6. writes dense immutable evidence for every eligible team in every submitted
   wave, including explicit zero rows for omitted teams;
7. calculates the leader-relative three-quarter-power performance for each
   wave and validates its unique-leader or tied-no-Crown assertion; and
8. averages relative performance and Crown share before applying the constant
   95/5 score when projecting or settling the epoch.

The arena remains persistent across ordinary round, Crown, and epoch
boundaries. A stopped managed runtime triggers immediate recovery; otherwise
three consecutive committed `Mumble` or `Offline` functional verdicts for the
same runtime trigger replacement. `InternalError` breaks that streak because
uncertain platform inspection must not destroy a healthy arena. Recovery
clears transient sessions and unsettled snapshots, preserves event
capabilities, and proves readiness before scoring resumes. Leaderboard mode
has a challenge-native per-wave Crown but does not use the Boot2Root marker,
provisional capture confirmation, scheduled Crown reset, or champion cooldown.
A snapshot must match the exact current round, lifecycle record, runtime
attempt, target, container, and objective schema; after the arrival window,
absence remains a field-wide void.
