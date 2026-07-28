# Signed KotH API observer

RSCTF can receive KotH control through a challenge-scoped API observer instead
of reading `/koth/king` with container exec. This is useful when the hill runs
on Kubernetes, a private worker, or another backend where exec-based marker
reads are undesirable.

The API is deliberately **not** a score webhook. It accepts only the current
team capability (or an explicit uncaptured state). RSCTF still runs the
functional checker, requires the configured consecutive healthy observations,
owns the crown-cycle lifecycle, and applies the fixed
`100R(0.25A + 0.55C + 0.20√AC)` formula.

The authentication shape is inspired by
[rCTF's signed dynamic-scoring webhook](https://github.com/otter-sec/rctf/blob/main/apps/docs/src/content/docs/api/challenges/submit-dynamic-scores.md),
but RSCTF does not accept externally calculated points.

## Security boundary

Run the observer as an independent trusted service. Do not put its HMAC secret
in the attacker-controlled hill image, environment, filesystem, or player
client. A team that compromises the hill is supposed to control the challenge;
it must not acquire the credential that speaks for the independent observer.

The public context is not a credential. It binds one request to the exact game,
challenge, target, crown cycle, reset attempt, and active container. A reset
changes the context, so a delayed signed request for the old container is
rejected.

Use HTTPS, synchronize the observer clock with NTP, keep the secret in a
restricted secret store, and rotate it after suspected disclosure. PostgreSQL
backups contain the symmetric observer key and must be protected accordingly.

## Enable an observer

1. Open the game's **A&D / KotH operations** page.
2. Select the **KotH** view.
3. In the hill's **Claim input** column, choose **Enable API**.
4. Create the observer secret and copy it immediately. RSCTF never returns the
   plaintext again.
5. Configure the independent observer with the game ID, challenge ID, RSCTF
   origin, and secret.
6. Start the official lifecycle while scoring is paused. As soon as the first
   active context is published, submit an explicit initial observation, verify
   it in the operator view, and then resume scoring.

The selected claim source is frozen in the official KotH snapshot. Enabling an
observer before that boundary selects `Api`; without a credential, the hill
uses `Marker`. The source cannot change after scoring starts. Secret rotation
and revocation remain available, but both clear the current API observation.
Pause scoring while rotating a live observer, submit fresh state, then resume.

## Protocol

Fetch the active context:

```http
GET /api/v1/koth/games/{gameId}/challenges/{challengeId}/context
```

The raw response uses camelCase and Unix-millisecond timestamps:

```json
{
  "apiVersion": "v1",
  "context": "64-lowercase-hex-characters",
  "cycleNumber": 4,
  "resetAttempt": 0,
  "generatedAt": 1785123456789
}
```

Submit the current observed capability:

```http
POST /api/v1/koth/games/{gameId}/challenges/{challengeId}/observations
Content-Type: application/json
X-RSCTF-Timestamp: 1785123456790
X-RSCTF-Signature: sha256=<lowercase hex HMAC-SHA256>

{"context":"<context>","token":"<exact current team capability>"}
```

Use JSON `null` to report an explicitly uncaptured hill:

```json
{"context":"<context>","token":null}
```

The `token` property is required; omitting it is rejected so a controller
cannot accidentally turn malformed state into an uncaptured observation.

Compute the signature over the exact bytes sent as the body:

```text
HMAC-SHA256(
  key = observer secret UTF-8 bytes,
  message = timestamp + "." + gameId + "." + challengeId + "." + rawBody
)
```

The timestamp must be a Unix-millisecond integer within five minutes of the
server. Each accepted timestamp must be newer than the previous observation in
the same active context. An accepted signature is replay-protected for ten
minutes. The body is limited to 1024 bytes, and capability text is limited to
256 bytes.

Typical responses are:

| Status | Meaning |
| --- | --- |
| `200` | The observation was accepted as checker input. No crown or points were awarded yet. |
| `400` | The signed JSON, context shape, or capability is invalid. |
| `401` | The credential, timestamp, or signature is invalid. These cases intentionally share one response. |
| `409` | The context changed, the timestamp is out of order, or an accepted request was replayed. Fetch context and retry with a new timestamp. |
| `429` | The source exceeded the platform request limit. Back off instead of flooding. |

Only a current, non-revoked capability for the exact hill/cycle/reset is
accepted. Eligibility and champion cooldown are rechecked by the authoritative
checker before the observation can affect crown state.

## Minimal Python client

This standard-library example sends one observation. A real observer should
derive `token` from its independent challenge logic, submit on every control
change, retry transient failures with backoff, and refetch context after `409`.
For a complete importable hill, functional checker, long-running observer, and
regression test, see
[`examples/challenge-repository/Koth/Web/api-observed-hill`](../../examples/challenge-repository/Koth/Web/api-observed-hill/).

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
    context = json.load(response)["context"]

# Replace this with the capability independently observed by your controller.
token = os.environ.get("RSCTF_KOTH_OBSERVED_TOKEN")
body = json.dumps(
    {"context": context, "token": token},
    separators=(",", ":"),
).encode()
timestamp = str(time.time_ns() // 1_000_000)
message = (
    f"{timestamp}.{GAME_ID}.{CHALLENGE_ID}.".encode() + body
)
signature = hmac.new(
    SECRET.encode(),
    message,
    hashlib.sha256,
).hexdigest()

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

## What the checker does next

At its server-randomized time, the checker:

1. resolves the snapshotted claim source;
2. reads the current exact-context API observation;
3. runs the independent functional checker;
4. reads the API observation again;
5. rejects an unstable or unavailable claim for holder election; and
6. feeds a stable capability into the existing provisional/confirmation state
   machine.

The immutable per-round evidence, reset voids, event deadline, personal
cooldown denominators, acquisition idempotency, and settled scoring path are
the same as marker mode.
