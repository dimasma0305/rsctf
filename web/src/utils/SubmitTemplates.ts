import JSZip from 'jszip'

/**
 * Downloadable challenge templates for the user-submit page.
 *
 * The static and dynamic-container examples mirror
 * `internal/template/templates/others/event-template/.example/*` from
 * https://github.com/dimasma0305/gzcli. The A&D package is rsctf-specific
 * because it includes the process-checker contract. All downloads are kept as
 * TypeScript string constants and generated client-side with JSZip.
 *
 * If gzcli's upstream templates change, sync the matching jeopardy examples.
 */

// ---------------------------------------------------------------------------
// static-attachment
// ---------------------------------------------------------------------------

const STATIC_ATTACHMENT_YAML = `name: "static-attachment"
author: "dimas"

# support markdown & html tags
description: |
  Example static attachment

type: "StaticAttachment" # don't touch this value
value: 1000 # don't touch this value

flags:
  - "flag{testing}"

provide: "./dist"
`

const STATIC_ATTACHMENT_FLAG = `flag{testing}
`

const STATIC_ATTACHMENT_SOLVER = `# example solver
`

const STATIC_ATTACHMENT_DIST_GITIGNORE = ``

// ---------------------------------------------------------------------------
// dynamic-container
// ---------------------------------------------------------------------------

const DYNAMIC_CONTAINER_YAML = `name: "dynamic-container"
author: "test"

# support markdown & html tags
description: |
  Testing dynamic container

type: "DynamicContainer" # don't touch this value
value: 1000 # don't touch this value

provide: "./dist"

container:
  flagTemplate: "FLAG{ini_test_flag_[TEAM_HASH]}"
  memoryLimit: 512
  cpuCount: 3
  storageLimit: 512
  exposePort: 8011
  enableTrafficCapture: true
`

const DYNAMIC_DOCKERFILE = `FROM python:3.9-alpine

RUN apk update && apk add socat

RUN adduser -D -u 1001 -s /bin/bash ctf

RUN mkdir /home/ctf/chall

COPY ./requirements.txt /home/ctf/chall
RUN pip3 install -r /home/ctf/chall/requirements.txt

RUN mkdir /home/ctf/chall/src

COPY ./chall.py /home/ctf/chall/src
COPY ./run.sh /home/ctf/chall/src

RUN chown -R root:root /home/ctf/chall
RUN chmod -R 555 /home/ctf/chall
USER ctf
WORKDIR /home/ctf/chall/src

CMD ["./run.sh"]
`

const DYNAMIC_RUN_SH = `#!/bin/sh

export FLAG=\${RSCTF_FLAG}
socat tcp-l:8011,reuseaddr,fork exec:"python3 chall.py"
`

const DYNAMIC_CHALL_PY = `# flag in env
print(__import__('os').popen('env').read())
`

const DYNAMIC_REQUIREMENTS = ``

const DYNAMIC_DOCKER_COMPOSE = `services:
  example:
    build: .
    restart: on-failure
    ports:
      - 8011:8011
    deploy:
      resources:
        limits:
          cpus: "0.5"
          memory: "256M"
        reservations:
          cpus: "0.25"
          memory: "128M"
`

const DYNAMIC_DIST_GITIGNORE = ``

const DYNAMIC_SOLVER = `# example solver
`

// ---------------------------------------------------------------------------
// attack-defense
// ---------------------------------------------------------------------------

const AD_CHALLENGE_YAML = `name: "attack-defense"
author: "test"

# support markdown & html tags
description: |
  Example Attack & Defense service. Every tick the platform writes a fresh
  flag to the path in RSCTF_FLAG_FILE. Read that file on every request so
  defenders can patch the bug without breaking the checker.

type: "AttackDefense" # don't touch this value

# TEAM_HASH identifies one team/challenge; GUID rotates every generated flag.
flagTemplate: "rsctf{ad_[TEAM_HASH]_[GUID]}"

# A&D reuses the container block for the per-team service and port.
# Keep containerImage omitted: importing this package builds ./src/Dockerfile.
container:
  exposePort: 80
  memoryLimit: 256
  cpuCount: 1
  storageLimit: 256

# A&D per-challenge knobs (this service's own properties). All optional.
# Event-wide policy — tick length, flag lifetime, reset cooldown, snapshot
# download — lives in the game's settings (admin → game → Info), not here.
ad:
  # false: rsctf runs one source-built service container per team.
  # true: each team runs that service through the BYOC/self-hosted workflow.
  # checker/run.py is identical in both modes; rsctf supplies its target IP/port.
  selfHosted: false
  # This controls service-container egress, not checker networking.
  allowEgress: false
  allowSelfReset: true

# checker/run.py is auto-detected and prepared as a sandboxed Python process.
# Repository Bindings installs exact requirements.txt pins from wheels only.
`

const AD_DOCKERFILE = `# Replace this with a pinned internal base when the build host must avoid
# Docker Hub.
FROM alpine:3.21

RUN apk add --no-cache socat

# Hosted mode uses /flag. BYOC supplies /shared/flag and sets RSCTF_FLAG_FILE
# accordingly. A creation-time RSCTF_FLAG value goes stale after rotation.
RUN echo 'flag{warmup-no-round-yet}' > /flag && chmod 0666 /flag
ENV RSCTF_FLAG_FILE=/flag

COPY serve.sh /serve.sh
RUN chmod +x /serve.sh

EXPOSE 80

# socat forks per connection; serve.sh reads RSCTF_FLAG_FILE every time.
CMD ["socat", "-T", "5", "TCP-LISTEN:80,reuseaddr,fork", "SYSTEM:/serve.sh"]
`

const AD_SERVE_SH = `#!/bin/sh
# Toy vulnerable service: exposes health and the current flag to anyone.
# Replace it with your service, but always read RSCTF_FLAG_FILE at request
# time. Hosted mode normally uses /flag; BYOC normally uses /shared/flag.
IFS= read -r request_line || true
request_line="\${request_line%$'\\r'}"
set -- $request_line
path="\${2:-/}"
while IFS= read -r line; do
    line="\${line%$'\\r'}"
    [ -z "$line" ] && break
done

status='200 OK'
case "$path" in
    /health)
        body='ok
'
        ;;
    /flag)
        flag_file="\${RSCTF_FLAG_FILE:-/flag}"
        flag="$(cat "$flag_file" 2>/dev/null || echo 'no flag yet')"
        body="\${flag}
"
        ;;
    /)
        body='rsctf A&D demo: inspect /flag
'
        ;;
    *)
        status='404 Not Found'
        body='not found
'
        ;;
esac

printf 'HTTP/1.1 %s\\r\\n' "$status"
printf 'Content-Type: text/plain\\r\\n'
printf 'Content-Length: %d\\r\\n' "\${#body}"
printf 'Connection: close\\r\\n'
printf '\\r\\n'
printf '%s' "$body"
`

const AD_CHECKER_LIB_PY = `"""Protocol-neutral, dependency-free helpers for rsctf process checkers.

Network and protocol code belongs in run.py. Copy this file together with
run.py; Repository Bindings prepares the whole checker directory, so sibling
imports work inside the checker sandbox.
"""

from dataclasses import dataclass
from enum import IntEnum
from functools import wraps
from ipaddress import ip_address
import os
import secrets
from typing import Callable, TypedDict, TypeVar


__all__ = [
    "AdContext",
    "KothContext",
    "Mumble",
    "Offline",
    "ad_checker",
    "checker",
    "koth_checker",
    "run_ad_checker",
    "run_koth_checker",
]


class Verdict(IntEnum):
    OK = 0
    MUMBLE = 1
    OFFLINE = 2
    INTERNAL_ERROR = 3


class Mumble(Exception):
    """The target answered, but its behavior was incorrect."""


class Offline(Exception):
    """The target could not provide a complete response."""


@dataclass(frozen=True)
class TargetContext:
    target_ip: str
    target_port: int
    round_number: int
    challenge_id: int


@dataclass(frozen=True)
class AdContext(TargetContext):
    # RSCTF_TEAM_ID currently contains the participation ID for A&D.
    participation_id: int
    flag: str


@dataclass(frozen=True)
class KothContext(TargetContext):
    pass


class _TargetValues(TypedDict):
    target_ip: str
    target_port: int
    round_number: int
    challenge_id: int


def _required(name: str) -> str:
    value = os.environ.get(name)
    if value is None or value == "":
        raise ValueError(f"missing {name}")
    return value


def _positive_integer(name: str, maximum: int | None = None) -> int:
    value = int(_required(name))
    if value <= 0 or (maximum is not None and value > maximum):
        raise ValueError(f"invalid {name}")
    return value


def _target_values() -> _TargetValues:
    if _required("RSCTF_ACTION").strip() != "check":
        raise ValueError("unsupported RSCTF_ACTION")
    return {
        "target_ip": str(ip_address(_required("RSCTF_TARGET_IP").strip())),
        "target_port": _positive_integer("RSCTF_TARGET_PORT", 65535),
        "round_number": _positive_integer("RSCTF_ROUND"),
        "challenge_id": _positive_integer("RSCTF_CHALLENGE_ID"),
    }


def _load_ad_context() -> AdContext:
    return AdContext(
        **_target_values(),
        participation_id=_positive_integer("RSCTF_TEAM_ID"),
        # Preserve the expected flag exactly; do not strip it.
        flag=_required("RSCTF_FLAG"),
    )


def _load_koth_context() -> KothContext:
    if int(_required("RSCTF_TEAM_ID")) != 0:
        raise ValueError("KotH checker expects RSCTF_TEAM_ID=0")
    if os.environ.get("RSCTF_FLAG") is not None:
        raise ValueError("KotH checker must not receive RSCTF_FLAG")
    return KothContext(**_target_values())


ContextT = TypeVar("ContextT", AdContext, KothContext)
CheckerFunctionT = TypeVar("CheckerFunctionT", bound=Callable[..., None])
_registered_checkers: list[Callable[..., object]] = []


def _execute(
    function: Callable[[ContextT], None],
    load_context: Callable[[], ContextT],
) -> int:
    try:
        context = load_context()
        result = function(context)
        if result is not None:
            raise TypeError("checker functions must return None")
    except Offline:
        return int(Verdict.OFFLINE)
    except Mumble:
        return int(Verdict.MUMBLE)
    except BaseException:
        # Configuration and checker bugs are infrastructure failures.
        return int(Verdict.INTERNAL_ERROR)
    return int(Verdict.OK)


def checker(function: CheckerFunctionT) -> CheckerFunctionT:
    """Register one focused check for the shuffled checker suite."""
    _registered_checkers.append(function)
    return function


def _shuffled_checkers() -> list[Callable[..., object]]:
    functions = list(_registered_checkers)
    for index in range(len(functions) - 1, 0, -1):
        selected = secrets.randbelow(index + 1)
        functions[index], functions[selected] = functions[selected], functions[index]
    return functions


def _failure_priority(error: BaseException) -> Verdict:
    if isinstance(error, Offline):
        return Verdict.OFFLINE
    if isinstance(error, Mumble):
        return Verdict.MUMBLE
    return Verdict.INTERNAL_ERROR


def _execute_registered(context: ContextT) -> None:
    functions = _shuffled_checkers()
    if not functions:
        raise RuntimeError("no checker functions registered")

    failures: list[BaseException] = []
    for function in functions:
        try:
            result = function(context)
            if result is not None:
                raise TypeError("checker functions must return None")
        except BaseException as error:
            failures.append(error)
    if failures:
        raise max(failures, key=_failure_priority)


def run_ad_checker() -> int:
    """Run every registered A&D check once in shuffled order."""
    return _execute(_execute_registered, _load_ad_context)


def run_koth_checker() -> int:
    """Run every registered KotH check once in shuffled order."""
    return _execute(_execute_registered, _load_koth_context)


def ad_checker(function: Callable[[AdContext], None]) -> Callable[[], int]:
    """Decorate one A&D check with context loading and verdict mapping."""

    @wraps(function)
    def wrapped() -> int:
        return _execute(function, _load_ad_context)

    return wrapped


def koth_checker(function: Callable[[KothContext], None]) -> Callable[[], int]:
    """Decorate one KotH check with context loading and verdict mapping."""

    @wraps(function)
    def wrapped() -> int:
        return _execute(function, _load_koth_context)

    return wrapped
`

const AD_CHECKER_RUN_PY = `"""Challenge-specific checks for the platform-hosted A&D demo."""

import httpx

from lib import AdContext, Mumble, Offline, checker, run_ad_checker


REQUEST_TIMEOUT_SECONDS = 3
MAX_RESPONSE_BYTES = 4096


# This demo speaks HTTP. A raw TCP, binary, or custom TCP challenge can replace
# this function without changing lib.py or the decorated check shape.
def http_get(context: AdContext, path: str) -> str:
    host = f"[{context.target_ip}]" if ":" in context.target_ip else context.target_ip
    url = f"http://{host}:{context.target_port}{path}"
    try:
        with httpx.Client(
            follow_redirects=False,
            timeout=REQUEST_TIMEOUT_SECONDS,
            trust_env=False,
        ) as client:
            with client.stream(
                "GET",
                url,
                headers={"Accept-Encoding": "identity", "Connection": "close"},
            ) as response:
                if response.status_code != 200:
                    raise Mumble(f"the service returned HTTP {response.status_code}")
                body = bytearray()
                for chunk in response.iter_raw(chunk_size=1024):
                    if len(body) + len(chunk) > MAX_RESPONSE_BYTES:
                        raise Mumble("the service response was too large")
                    body.extend(chunk)
    except Mumble:
        raise
    except (httpx.TimeoutException, httpx.NetworkError) as error:
        raise Offline("the service did not complete the request") from error
    except httpx.ProtocolError as error:
        raise Mumble("the service returned invalid HTTP") from error

    try:
        return body.decode("utf-8").rstrip("\\r\\n")
    except UnicodeDecodeError as error:
        raise Mumble("the service response was not UTF-8") from error


@checker
def check_health(context: AdContext) -> None:
    if http_get(context, "/health") != "ok":
        raise Mumble("the health endpoint did not return ok")


@checker
def check_flag(context: AdContext) -> None:
    if http_get(context, "/flag") != context.flag:
        raise Mumble("the flag endpoint did not return this round's flag")


if __name__ == "__main__":
    raise SystemExit(run_ad_checker())
`

const AD_CHECKER_REQUIREMENTS = `httpx==0.28.1
`

const AD_CHECKER_README = `# A&D checker template

Copy \`lib.py\`, \`run.py\`, and \`requirements.txt\` together. Repository
Bindings prepares the checker virtual environment and installs the exact
\`httpx==0.28.1\` pin before publishing the immutable checker revision.

- \`run.py\` owns the challenge protocol and focused functional checks. This
  example disables redirects and proxy-environment use, requests identity
  encoding, applies a short timeout, and streams at most 4096 response bytes.
- \`lib.py\` is deliberately protocol-neutral. It only owns environment and
  context validation, verdict exceptions, shuffled suite ordering, and exit-code
  mapping. Copy it unchanged when starting another checker.
- \`requirements.txt\` contains simple exact \`name==version\` pins. Repository
  Bindings accepts comments and blank lines but rejects URLs, local paths, pip
  options, ranges, editable installs, and packages without compatible wheels.

Only the exit code is evaluated:

- 0: OK
- 1: Mumble (reachable, but incorrect)
- 2: Offline (connection failure or timeout)
- 3: InternalError (invalid context or checker bug)

Every \`@checker\` function runs exactly once per invocation. A cryptographic
Fisher–Yates shuffle changes only their order; it never selects or skips a
function. The runner attempts the full set even after a failure, then reports
the most severe result: InternalError, Offline, Mumble, or OK. Keep flag
validation in at least one registered A&D check. Checks must be read-only and
must not depend on another check running first; an independently shuffled run
may still repeat the previous order. \`run_ad_checker()\` loads the context,
while the original \`@ad_checker\` and \`@koth_checker\` single-function APIs
remain available for simple checkers. Keep the whole suite within the checker
deadline: the platform's outer hard timeout can terminate an overlong process.

Return normally for OK, or raise \`Mumble\`/\`Offline\` from protocol code. The
sandbox's outbound TCP connections can only reach
\`context.target_ip:context.target_port\` (the validated
\`RSCTF_TARGET_IP:RSCTF_TARGET_PORT\` pair), and stdout/stderr are discarded.

A&D checkers receive the already-delivered flag as \`context.flag\`. Verify it
through normal service behavior without changing service state. The same
checker works for platform-hosted and self-hosted/BYOC services. Preparing this
template requires the trusted rsctf scanner or approving administrator to reach
PyPI and its package-file hosts; review every pin before import.
`

const AD_SOLVER = `# Example A&D exploit.
#
# In Attack & Defense your "solver" is the exploit you run against OTHER
# teams' instances of this service each tick, then submit the captured
# flags via the API (see the in-game Toolkit -> "How to submit").
#
#   import requests
#   flag = requests.get(f"http://{target_ip}/flag", timeout=5).text
#   # POST flag to /api/Game/{id}/Ad/Submit with your Bearer token
`

// ---------------------------------------------------------------------------
// Build / download helpers
// ---------------------------------------------------------------------------

/**
 * Static attachment template — no container, no build. The player
 * downloads whatever lives in `dist/`; flag is matched server-side
 * from the `flags:` list.
 */
export async function buildStaticAttachmentTemplate(): Promise<Blob> {
  const zip = new JSZip()
  zip.file('challenge.yml', STATIC_ATTACHMENT_YAML)
  zip.file('src/flag.txt', STATIC_ATTACHMENT_FLAG)
  zip.file('dist/.gitignore', STATIC_ATTACHMENT_DIST_GITIGNORE)
  zip.file('solver/solve.py', STATIC_ATTACHMENT_SOLVER)
  return zip.generateAsync({ type: 'blob', compression: 'DEFLATE' })
}

/**
 * Dynamic container template — one container per team, flag injected
 * via the RSCTF_FLAG env var (no `flags:` block + a `flagTemplate`).
 */
export async function buildDynamicContainerTemplate(): Promise<Blob> {
  const zip = new JSZip()
  zip.file('challenge.yml', DYNAMIC_CONTAINER_YAML)
  zip.file('src/Dockerfile', DYNAMIC_DOCKERFILE)
  zip.file('src/run.sh', DYNAMIC_RUN_SH)
  zip.file('src/chall.py', DYNAMIC_CHALL_PY)
  zip.file('src/requirements.txt', DYNAMIC_REQUIREMENTS)
  zip.file('src/docker-compose.yml', DYNAMIC_DOCKER_COMPOSE)
  zip.file('dist/.gitignore', DYNAMIC_DIST_GITIGNORE)
  zip.file('solver/solve.py', DYNAMIC_SOLVER)
  return zip.generateAsync({ type: 'blob', compression: 'DEFLATE' })
}

/**
 * Attack & Defense template — a persistent per-team service plus a
 * process checker. The platform expands `flagTemplate`, writes the fresh
 * value to `RSCTF_FLAG_FILE` every tick, and passes it to the checker. Ships
 * the source-built service under `src/`, a pinned Python checker under
 * `checker/`, and an exploit stub under `solver/`.
 */
export async function buildAttackDefenseTemplate(): Promise<Blob> {
  const zip = new JSZip()
  zip.file('challenge.yml', AD_CHALLENGE_YAML)
  zip.file('src/Dockerfile', AD_DOCKERFILE)
  zip.file('src/serve.sh', AD_SERVE_SH)
  zip.file('checker/lib.py', AD_CHECKER_LIB_PY)
  zip.file('checker/run.py', AD_CHECKER_RUN_PY)
  zip.file('checker/requirements.txt', AD_CHECKER_REQUIREMENTS)
  zip.file('checker/README.md', AD_CHECKER_README)
  zip.file('solver/solve.py', AD_SOLVER)
  return zip.generateAsync({ type: 'blob', compression: 'DEFLATE' })
}

/**
 * Trigger a browser download for the given blob.
 */
export function downloadBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  document.body.appendChild(a)
  a.click()
  document.body.removeChild(a)
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}
