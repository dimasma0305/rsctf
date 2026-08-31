// Materialize the Python entrypoints required by rsctf's checker contract. The load
// harness remains JavaScript-only; Node owns creation of these short-lived fixtures.
import { chmodSync, mkdirSync, writeFileSync } from 'node:fs';

const ROOT = process.env.LOAD_FIXTURE_ROOT || '/tmp/rsctf-load-fixtures';

const CHECKER = String.raw`"""Dependency-free exact checker for the lifecycle A&D fixture."""

import os
import socket
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import urlopen


def fetch(url: str) -> str:
    with urlopen(url, timeout=5) as response:
        return response.read(1024).decode("utf-8").strip()


def main() -> int:
    try:
        host = os.environ["RSCTF_TARGET_IP"].strip()
        port = int(os.environ["RSCTF_TARGET_PORT"])
        team = os.environ["RSCTF_TEAM_ID"].strip()
        flag = os.environ["RSCTF_FLAG"]
    except (KeyError, TypeError, ValueError):
        return 3

    if not host or not team or not flag or not 1 <= port <= 65535:
        return 3

    base = f"http://{host}:{port}"
    try:
        observed = fetch(f"{base}/flag?{urlencode({'team': team})}")
        if observed == flag:
            return 0
        # The shared bootstrap fixture has no relay-owned flag volume, so it
        # retains a checker-only planting fallback. Isolated event services
        # reject /plant and validate only the real relay publication path.
        planted = fetch(f"{base}/plant?{urlencode({'team': team, 'flag': flag})}")
        if planted != "ok":
            return 1
        observed = fetch(f"{base}/flag?{urlencode({'team': team})}")
        return 0 if observed == flag else 1
    except HTTPError:
        return 1
    except (URLError, TimeoutError, ConnectionError, socket.timeout, OSError):
        return 2
    except Exception:
        return 3


raise SystemExit(main())
`;

const KOTH_CHECKER = String.raw`"""Functional readiness/SLA checker for the lifecycle KotH hill."""

import os
import socket
from urllib.error import HTTPError, URLError
from urllib.request import urlopen


def main() -> int:
    try:
        host = os.environ["RSCTF_TARGET_IP"].strip()
        port = int(os.environ["RSCTF_TARGET_PORT"])
    except (KeyError, TypeError, ValueError):
        return 3
    if not host or not 1 <= port <= 65535:
        return 3
    try:
        with urlopen(f"http://{host}:{port}/", timeout=5) as response:
            body = response.read(4096)
            return 0 if response.status == 200 and body == b"RSCTF competitive hill\n" else 1
    except HTTPError as error:
        return 2 if error.code >= 500 else 1
    except (URLError, TimeoutError, ConnectionError, socket.timeout, OSError):
        return 2
    except Exception:
        return 3


raise SystemExit(main())
`;

const SERVICE = String.raw`"""Shared exact-flag service used behind every lifecycle BYOC tunnel."""

import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit


flags: dict[str, str] = {}
flags_lock = threading.Lock()
flag_file = os.environ.get("FLAG_FILE", "").strip()
defense_key = os.environ.get("DEFENSE_KEY", "").strip()
patch_level = 0
service_state = "healthy"
patch_lock = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        global patch_level, service_state
        request = urlsplit(self.path)
        values = parse_qs(request.query, keep_blank_values=True)
        team = values.get("team", [""])[0]
        status = 200

        if request.path == "/health":
            body = b"ok\n"
        elif request.path in ("/flag", "/exploit") and team:
            technique_text = values.get("technique", ["1"])[0]
            try:
                technique = int(technique_text)
            except ValueError:
                technique = -1
            with patch_lock:
                current_patch = patch_level
                current_state = service_state
            if current_state == "offline":
                status = 503
                body = b"service offline after patch\n"
            elif current_state == "mumble":
                body = b"service-mumble\n"
            elif request.path == "/exploit" and (not 1 <= technique <= 3 or technique <= current_patch):
                status = 403
                body = b"patched\n"
            elif flag_file:
                try:
                    with open(flag_file, "rb") as current:
                        body = current.read(257).strip() + b"\n"
                except OSError:
                    body = b"flag-not-planted-yet\n"
            else:
                with flags_lock:
                    body = (flags.get(team, "flag-not-planted-yet") + "\n").encode()
        elif request.path == "/defense":
            supplied_key = self.headers.get("X-Defense-Key", "")
            repair = values.get("repair", [""])[0] == "1"
            incident = values.get("incident", ["healthy"])[0]
            level_text = values.get("level", [""])[0]
            try:
                level = int(level_text)
            except ValueError:
                level = -1
            if not defense_key or supplied_key != defense_key:
                status = 403
                body = b"forbidden\n"
            elif repair:
                with patch_lock:
                    service_state = "healthy"
                body = b"repaired\n"
            elif not 0 <= level <= 2 or incident not in ("healthy", "mumble", "offline"):
                status = 400
                body = b"invalid defense update\n"
            else:
                with patch_lock:
                    patch_level = level
                    service_state = incident
                body = f"patch={level};state={incident}\n".encode()
        elif request.path == "/plant" and team:
            if flag_file:
                status = 405
                body = b"relay publication required\n"
                self.send_response(status)
                self.send_header("Content-Type", "text/plain; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            flag = values.get("flag", [""])[0]
            if not flag or len(flag) > 256 or "\n" in flag or "\r" in flag:
                status = 400
                body = b"invalid flag\n"
            else:
                with flags_lock:
                    flags[team] = flag
                body = b"ok\n"
        else:
            status = 404
            body = b"not found\n"

        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, _format, *_args):
        pass


port = int(os.environ.get("PORT", "8080"))
ThreadingHTTPServer(("0.0.0.0", port), Handler).serve_forever()
`;

const KOTH_SERVICE = String.raw`"""Network-capturable KotH fixture used only by the lifecycle harness."""

import os
import re
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit


TOKEN = re.compile(rb"^koth_[A-Za-z0-9_-]{8,128}$")
KING_PATH = os.environ.get("KOTH_KING_PATH", "/koth/king")
KING_DIRECTORY = os.path.dirname(KING_PATH)
marker_lock = threading.Lock()
patch_level = 0
service_state = "healthy"
instance_id = os.urandom(8).hex()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        global patch_level, service_state
        request = urlsplit(self.path)
        if request.path == "/capture":
            token = self.headers.get("X-Koth-Token", "").encode()
            values = parse_qs(request.query, keep_blank_values=True)
            try:
                technique = int(values.get("technique", ["3"])[0])
            except ValueError:
                technique = -1
            self.capture(token, technique)
            return
        if request.path == "/defense":
            values = parse_qs(request.query, keep_blank_values=True)
            token = self.headers.get("X-Koth-Token", "").encode()
            self.defend(token, values)
            return
        if request.path == "/status":
            with marker_lock:
                body = f"instance={instance_id};patch={patch_level};state={service_state}\n".encode()
            self.send_response(200)
            self.send_header("X-Koth-Instance", instance_id)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if request.path != "/":
            self.send_error(404)
            return
        with marker_lock:
            current_state = service_state
        if current_state == "offline":
            status = 503
            body = b"service-offline\n"
        elif current_state == "mumble":
            status = 200
            body = b"RSCTF hill degraded\n"
        else:
            status = 200
            body = b"RSCTF competitive hill\n"
        self.send_response(status)
        self.send_header("X-Koth-Instance", instance_id)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if urlsplit(self.path).path != "/capture":
            self.send_error(404)
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = 0
        self.capture(self.rfile.read(length).strip() if 0 < length <= 256 else b"", 3)

    def current_token(self):
        try:
            with open(KING_PATH, "rb") as marker:
                return marker.read(256).strip()
        except OSError:
            return b""

    def defend(self, token, values):
        global patch_level, service_state
        if not TOKEN.fullmatch(token):
            self.send_error(400)
            return
        repair = values.get("repair", [""])[0] == "1"
        incident = values.get("incident", ["healthy"])[0]
        try:
            level = int(values.get("level", ["-1"])[0])
        except ValueError:
            level = -1
        with marker_lock:
            if token != self.current_token():
                self.send_error(403)
                return
            if repair:
                service_state = "healthy"
                body = f"patch={patch_level};state=healthy\n".encode()
            elif not 1 <= level <= 2 or incident not in ("healthy", "mumble", "offline"):
                self.send_error(400)
                return
            else:
                patch_level = max(patch_level, level)
                service_state = incident
                body = f"patch={patch_level};state={service_state}\n".encode()
        self.send_response(200)
        self.send_header("X-Koth-Instance", instance_id)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def capture(self, token, technique):
        global patch_level, service_state
        if not TOKEN.fullmatch(token):
            self.send_error(400)
            return
        if not 1 <= technique <= 3:
            self.send_error(400)
            return
        with marker_lock:
            if service_state == "offline":
                body = b"service-offline\n"
                self.send_response(503)
                self.send_header("X-Koth-Defense", "offline")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            if service_state == "mumble":
                body = b"service-mumble\n"
                self.send_response(409)
                self.send_header("X-Koth-Defense", "mumble")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            if technique <= patch_level:
                body = b"patched\n"
                self.send_response(403)
                self.send_header("X-Koth-Defense", "blocked")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            defense = "bypassed" if patch_level > 0 else "none"
            os.makedirs(KING_DIRECTORY, exist_ok=True)
            temporary = None
            try:
                with tempfile.NamedTemporaryFile(dir=KING_DIRECTORY, prefix=".king-", delete=False) as marker:
                    temporary = marker.name
                    marker.write(token)
                os.replace(temporary, KING_PATH)
                temporary = None
            finally:
                if temporary and os.path.exists(temporary):
                    os.unlink(temporary)
        # The atomic rename is the commit point. A bodyless, explicitly flushed
        # response minimizes the interval in which a cycle reset can destroy a
        # successfully captured hill before the player receives its receipt.
        self.send_response(204)
        self.send_header("X-Koth-Defense", defense)
        self.send_header("Content-Length", "0")
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.flush()
        self.close_connection = True

    def log_message(self, _format, *_args):
        pass


class CaptureServer(ThreadingHTTPServer):
    # A 100-team opening can produce a short connection burst. The standard
    # backlog is only five, which resets otherwise valid captures before a
    # handler thread starts. Keep the queue bounded but large enough for one
    # full event roster, and never make reset wait for old request threads.
    request_queue_size = 128
    daemon_threads = True


port = int(os.environ.get("PORT", "8080"))
CaptureServer(("0.0.0.0", port), Handler).serve_forever()
`;

const MANAGED_KOTH_SERVICE = String.raw`"""Managed Leaderboard KotH fixture with an in-target RSCTF reporter."""

import hashlib
import hmac
import json
import os
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


MAX_REQUEST_BYTES = 2_048
MAX_SCORE = 1_000
OBJECTIVE_IDS = ["official-score"]
ACTIVE_FLEET = int(os.environ.get("RSCTF_LOAD_ACTIVE_FLEET", "64"))
if not 2 <= ACTIVE_FLEET <= 128:
    raise RuntimeError("RSCTF_LOAD_ACTIVE_FLEET must be between 2 and 128")

REPORTER_ENV_NAMES = (
    "RSCTF_KOTH_GAME_ID",
    "RSCTF_KOTH_CHALLENGE_ID",
    "RSCTF_KOTH_PLATFORM_URL",
    "RSCTF_KOTH_CONTEXT_URL",
    "RSCTF_KOTH_OBSERVATION_URL",
    "RSCTF_KOTH_REPORTER_SECRET",
)
reporter_values = [os.environ.get(name, "").strip() for name in REPORTER_ENV_NAMES]
if any(reporter_values) and not all(reporter_values):
    raise RuntimeError("managed reporter environment is incomplete")
REPORTER_CONFIGURED = all(reporter_values)
if REPORTER_CONFIGURED:
    GAME_ID = int(reporter_values[0])
    CHALLENGE_ID = int(reporter_values[1])
    PLATFORM_URL = reporter_values[2].rstrip("/")
    CONTEXT_URL = reporter_values[3]
    OBSERVATION_URL = reporter_values[4]
    REPORTER_SECRET = reporter_values[5]
    AUTH_URL = f"{PLATFORM_URL}/api/v1/koth/capability/authenticate"
else:
    GAME_ID = 0
    CHALLENGE_ID = 0
    PLATFORM_URL = ""
    CONTEXT_URL = ""
    OBSERVATION_URL = ""
    REPORTER_SECRET = ""
    AUTH_URL = ""

state_lock = threading.Lock()
authenticated_scores = {}
active_hashes = set()
invalid_authentications = 0
successful_reports = 0
submitted_waves = 0
context_refreshes = 0
eligible_roster = 0
last_round = 0
last_error = None
reported_context = None


def compact_json(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode()


def request_json(url, *, body=None, headers=None, timeout=5):
    payload = None if body is None else compact_json(body)
    request = Request(
        url,
        data=payload,
        method="GET" if payload is None else "POST",
        headers={"Content-Type": "application/json", **(headers or {})},
    )
    try:
        with urlopen(request, timeout=timeout) as response:
            raw = response.read(524_289)
            if len(raw) > 524_288:
                raise RuntimeError("RSCTF response exceeded the fixture bound")
            response_headers = {name.lower(): value for name, value in response.headers.items()}
            return response.status, json.loads(raw), response_headers
    except HTTPError as error:
        error.read(65_537)
        error.close()
        raise


def vary_has_api_version(headers):
    return "x-rsctf-api-version" in {
        item.strip().lower()
        for item in headers.get("vary", "").split(",")
    }


def unwrap_model(value):
    if not isinstance(value, dict):
        raise RuntimeError("RSCTF returned a non-object response")
    model = value.get("data", value)
    if not isinstance(model, dict):
        raise RuntimeError("RSCTF returned a non-object data envelope")
    return model


def authenticate_capability(token):
    expected_hash = hashlib.sha256(token.encode()).hexdigest()
    status, model, _ = request_json(
        AUTH_URL,
        body={"token": token, "gameId": GAME_ID, "challengeId": CHALLENGE_ID},
    )
    model = unwrap_model(model)
    if status != 200 or model.get("teamId") != expected_hash or not model.get("teamName"):
        raise RuntimeError("RSCTF returned an invalid capability identity")
    return expected_hash


def evidence_rows(eligible_hashes, scores):
    # Every finalized wave is dense at the platform's 2,000-team body limit.
    # Authentication exercises every frozen capability, while only the bounded
    # arena fleet contributes a positive challenge-native score.
    ranked = [(token_hash, int(scores[token_hash])) for token_hash in eligible_hashes]
    if sum(1 for _, score in ranked if score > 0) != ACTIVE_FLEET:
        raise RuntimeError("managed reporter requires the exact bounded positive cohort")
    highest = max((score for _, score in ranked), default=0)
    leaders = {token_hash for token_hash, score in ranked if score == highest and score > 0}
    if len(leaders) != 1:
        raise RuntimeError("managed reporter scores must choose one unique Crown")
    unique_leader = next(iter(leaders))
    rows = []
    for token_hash, score in ranked:
        rows.append({
            "tokenHash": token_hash,
            "activity": {"earned": 1, "possible": 1},
            "objectives": [{"earned": score, "possible": MAX_SCORE}],
            "isCrown": token_hash == unique_leader,
        })
    return rows


def wave_times(context, count):
    window_start = int(context["waveWindowStartsAt"])
    window_end = int(context["waveWindowEndsAt"])
    available_end = min(int(time.time_ns() // 1_000_000), window_end - 1)
    # This fixture defines one-second challenge-native waves. Each scoring
    # context carries one complete finalized wave; later rounds carry later
    # waves without exceeding the 2,000 team-wave snapshot ceiling.
    ends = [window_start + 1_000 * (index + 1) for index in range(count)]
    if not ends or available_end < ends[-1] or ends[-1] >= window_end:
        return None
    return ends


def reporter_once():
    global active_hashes, last_error, last_round, reported_context
    global context_refreshes, eligible_roster, successful_reports, submitted_waves
    _, context, context_headers = request_json(
        CONTEXT_URL,
        headers={"X-RSCTF-API-Version": "v2"},
    )
    context = unwrap_model(context)
    eligible = context.get("eligibleTokenHashes")
    if (
        context.get("apiVersion") != "v2"
        or not isinstance(eligible, list)
        or not 1 <= len(eligible) <= 2_000
        or len(set(eligible)) != len(eligible)
        or any(not isinstance(item, str) or len(item) != 64 for item in eligible)
        or context.get("objectiveIds") not in ([], OBJECTIVE_IDS)
        or context_headers.get("cache-control") != "no-store"
        or not vary_has_api_version(context_headers)
    ):
        raise RuntimeError("managed reporter received an invalid 2,000-team context")
    ordered = sorted(eligible)
    with state_lock:
        context_refreshes += 1
        eligible_roster = len(ordered)
        for stale_hash in set(authenticated_scores).difference(eligible):
            del authenticated_scores[stale_hash]
        active_hashes.intersection_update(eligible)
        selected = ordered
        ready = (
            all(token_hash in authenticated_scores for token_hash in selected)
            and len(active_hashes) == ACTIVE_FLEET
        )
        already_reported = (
            reported_context == context.get("context")
            or int(context.get("roundNumber", 0)) <= last_round
        )
        scores = {token_hash: authenticated_scores[token_hash] for token_hash in selected if token_hash in authenticated_scores}
        # A successful context read proves callback health even while the arena
        # waits for its bounded active cohort or a wave boundary.
        last_error = None
    if not ready or already_reported:
        return
    ended = wave_times(context, 1)
    if ended is None:
        return
    rows = evidence_rows(selected, scores)
    waves = [{
        "waveId": f"load-{context['resetAttempt']}-{context['roundNumber']}-dense",
        "endedAtUnixMs": ended[0],
        "teams": rows,
    }]
    raw_body = compact_json({
        "context": context["context"],
        "objectiveIds": OBJECTIVE_IDS,
        "waves": waves,
    })
    if len(raw_body) > 512 * 1_024:
        raise RuntimeError("managed reporter snapshot exceeded 512 KiB")
    timestamp = str(time.time_ns() // 1_000_000)
    message = f"{timestamp}.{GAME_ID}.{CHALLENGE_ID}.".encode() + raw_body
    signature = hmac.new(REPORTER_SECRET.encode(), message, hashlib.sha256).hexdigest()
    request = Request(
        OBSERVATION_URL,
        data=raw_body,
        method="POST",
        headers={
            "Content-Type": "application/json",
            "X-RSCTF-Timestamp": timestamp,
            "X-RSCTF-Signature": f"sha256={signature}",
        },
    )
    with urlopen(request, timeout=5) as response:
        accepted = unwrap_model(json.loads(response.read(65_537)))
        accepted_at = accepted.get("acceptedAt")
        if (
            response.status != 200
            or set(accepted) != {
                "accepted", "cycleNumber", "resetAttempt", "roundNumber",
                "submittedWaves", "submittedTeams", "recognizedTeams", "acceptedAt",
            }
            or accepted.get("accepted") is not True
            or accepted.get("cycleNumber") != context["cycleNumber"]
            or accepted.get("resetAttempt") != context["resetAttempt"]
            or accepted.get("roundNumber") != context["roundNumber"]
            or accepted.get("submittedWaves") != len(waves)
            or accepted.get("submittedTeams") != len(selected)
            or accepted.get("recognizedTeams") != len(selected)
            or type(accepted_at) is not int
            or abs((time.time_ns() // 1_000_000) - accepted_at) > 120_000
        ):
            raise RuntimeError("managed reporter acknowledgement was inconsistent")
    _, frozen_context, frozen_headers = request_json(
        CONTEXT_URL,
        headers={"X-RSCTF-API-Version": "v2"},
    )
    frozen_context = unwrap_model(frozen_context)
    if (
        frozen_context.get("objectiveIds") != OBJECTIVE_IDS
        or not isinstance(frozen_context.get("objectiveSchemaHash"), str)
        or len(frozen_context["objectiveSchemaHash"]) != 64
        or (
            context.get("objectiveIds") == []
            and frozen_context.get("context") == context.get("context")
        )
        or (
            context.get("objectiveIds") == OBJECTIVE_IDS
            and frozen_context.get("context") != context.get("context")
        )
        or frozen_context.get("cycleNumber") != context.get("cycleNumber")
        or frozen_context.get("resetAttempt") != context.get("resetAttempt")
        or frozen_context.get("roundNumber") != context.get("roundNumber")
        or frozen_context.get("eligibleTokenHashes") != eligible
        or frozen_headers.get("cache-control") != "no-store"
        or not vary_has_api_version(frozen_headers)
    ):
        raise RuntimeError("managed reporter objective schema did not freeze")
    with state_lock:
        reported_context = frozen_context["context"]
        successful_reports += 1
        submitted_waves += len(waves)
        last_round = int(context["roundNumber"])
        last_error = None


def reporter_loop():
    global last_error
    while True:
        try:
            reporter_once()
        except HTTPError as error:
            # Context changes and rate admission are expected bounded retries;
            # every other response remains visible in the secret-free status.
            with state_lock:
                last_error = f"HTTP {error.code}"
        except (URLError, TimeoutError, ConnectionError, OSError, ValueError, RuntimeError) as error:
            with state_lock:
                last_error = type(error).__name__
        time.sleep(1.0)


class Handler(BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(5)

    def send_json(self, status, model, headers=None):
        body = compact_json(model)
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/":
            body = b"RSCTF competitive hill\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path == "/healthz":
            body = b"ok"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path == "/reporter-status":
            with state_lock:
                model = {
                    "reporterConfigured": REPORTER_CONFIGURED,
                    "reporterHealthy": last_error is None,
                    "successfulReports": successful_reports,
                    "submittedWaves": submitted_waves,
                    "contextRefreshes": context_refreshes,
                    "eligibleRoster": eligible_roster,
                    "uniqueAuthenticated": len(authenticated_scores),
                    "uniqueActivePlayed": len(active_hashes.intersection(authenticated_scores)),
                    "invalidAuthentications": invalid_authentications,
                    "lastRound": last_round,
                    "lastContext": None,
                    "lastError": last_error,
                }
            self.send_json(200, model)
            return
        self.send_error(404)

    def do_POST(self):
        global invalid_authentications
        if self.path != "/play":
            self.send_error(404)
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            length = 0
        if not 1 <= length <= MAX_REQUEST_BYTES:
            self.send_json(400, {"accepted": False})
            return
        try:
            model = json.loads(self.rfile.read(length))
            if not isinstance(model, dict):
                raise ValueError("invalid play")
            token = model.get("token", "")
            score = model.get("score", -1)
            if (
                not isinstance(token, str)
                or not token.startswith("koth_")
                or type(score) is not int
                or not 0 <= score <= MAX_SCORE
            ):
                raise ValueError("invalid play")
            if not REPORTER_CONFIGURED:
                self.send_json(503, {"accepted": False})
                return
            team_id = authenticate_capability(token)
            token = None
        except HTTPError as error:
            if error.code in (401, 429):
                with state_lock:
                    invalid_authentications += 1
                response_headers = {}
                if error.code == 429:
                    retry_after = error.headers.get("Retry-After", "1")
                    response_headers["Retry-After"] = retry_after
                self.send_json(error.code, {"accepted": False}, response_headers)
                return
            self.send_json(503, {"accepted": False})
            return
        except (URLError, TimeoutError, ConnectionError, OSError):
            self.send_json(503, {"accepted": False})
            return
        except RuntimeError:
            self.send_json(503, {"accepted": False})
            return
        except (TypeError, ValueError, json.JSONDecodeError):
            self.send_json(400, {"accepted": False})
            return
        with state_lock:
            if score > 0 and team_id not in active_hashes and len(active_hashes) >= ACTIVE_FLEET:
                self.send_json(409, {"accepted": False})
                return
            authenticated_scores[team_id] = score
            if score > 0:
                active_hashes.add(team_id)
            else:
                active_hashes.discard(team_id)
            scoreable = score > 0
        self.send_json(200, {"accepted": True, "teamId": team_id, "scoreable": scoreable})

    def log_message(self, _format, *_args):
        pass


class ManagedServer(ThreadingHTTPServer):
    request_queue_size = 256
    daemon_threads = True
    worker_slots = threading.BoundedSemaphore(128)

    def process_request(self, request, client_address):
        if not self.worker_slots.acquire(timeout=1):
            request.close()
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.worker_slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.worker_slots.release()


if REPORTER_CONFIGURED:
    threading.Thread(target=reporter_loop, name="managed-reporter", daemon=True).start()
port = int(os.environ.get("PORT", "8080"))
ManagedServer(("0.0.0.0", port), Handler).serve_forever()
`;

const KOTH_DOCKERFILE = [
  'ARG BASE_IMAGE',
  'FROM ${BASE_IMAGE}',
  'COPY koth-service.py /opt/rsctf-load/koth-service.py',
  'EXPOSE 8080',
  'HEALTHCHECK --interval=10s --timeout=5s --start-period=20s --retries=6 CMD python3 -c "import urllib.request; urllib.request.urlopen(\'http://127.0.0.1:8080/\', timeout=3).read()"',
  'ENTRYPOINT ["python3", "/opt/rsctf-load/koth-service.py"]',
  '',
].join('\n');

const MANAGED_KOTH_DOCKERFILE = [
  'ARG BASE_IMAGE',
  'FROM ${BASE_IMAGE}',
  'LABEL rsctf.load.fixture="managed-koth-v1"',
  'COPY managed-koth-service.py /opt/rsctf-load/managed-koth-service.py',
  'ENV RSCTF_LOAD_ACTIVE_FLEET=64',
  'EXPOSE 8080',
  'HEALTHCHECK --interval=5s --timeout=3s --start-period=10s --retries=6 CMD python3 -c "import urllib.request; urllib.request.urlopen(\'http://127.0.0.1:8080/healthz\', timeout=2).read()"',
  'ENTRYPOINT ["python3", "/opt/rsctf-load/managed-koth-service.py"]',
  '',
].join('\n');

const AD_DOCKERFILE = [
  'ARG BASE_IMAGE',
  'FROM ${BASE_IMAGE}',
  'LABEL rsctf.load.fixture="managed-ad-v1"',
  'COPY ad-service.py /opt/rsctf-load/ad-service.py',
  'ENV FLAG_FILE=/flag',
  'EXPOSE 8080',
  'HEALTHCHECK --interval=10s --timeout=5s --start-period=20s --retries=6 CMD python3 -c "import urllib.request; urllib.request.urlopen(\'http://127.0.0.1:8080/health\', timeout=3).read()"',
  'ENTRYPOINT ["python3", "/opt/rsctf-load/ad-service.py"]',
  '',
].join('\n');

function writeFixture(path, contents) {
  writeFileSync(path, contents, { mode: 0o644 });
  chmodSync(path, 0o644);
}

export function materializeFixtures() {
  mkdirSync(ROOT, { recursive: true, mode: 0o755 });
  const checker = `${ROOT}/ad-checker.py`;
  const kothChecker = `${ROOT}/koth-checker.py`;
  const service = `${ROOT}/ad-service.py`;
  const kothService = `${ROOT}/koth-service.py`;
  const managedKothService = `${ROOT}/managed-koth-service.py`;
  const adDockerfile = `${ROOT}/Dockerfile.ad`;
  const kothDockerfile = `${ROOT}/Dockerfile.koth`;
  const managedKothDockerfile = `${ROOT}/Dockerfile.managed-koth`;
  writeFixture(checker, CHECKER);
  writeFixture(kothChecker, KOTH_CHECKER);
  writeFixture(service, SERVICE);
  writeFixture(kothService, KOTH_SERVICE);
  writeFixture(managedKothService, MANAGED_KOTH_SERVICE);
  writeFixture(adDockerfile, AD_DOCKERFILE);
  writeFixture(kothDockerfile, KOTH_DOCKERFILE);
  writeFixture(managedKothDockerfile, MANAGED_KOTH_DOCKERFILE);
  return {
    checker,
    kothChecker,
    service,
    kothService,
    managedKothService,
    adDockerfile,
    kothDockerfile,
    managedKothDockerfile,
    root: ROOT,
  };
}
