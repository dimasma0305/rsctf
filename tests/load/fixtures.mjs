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

const KOTH_DOCKERFILE = [
  'ARG BASE_IMAGE',
  'FROM ${BASE_IMAGE}',
  'COPY koth-service.py /opt/rsctf-load/koth-service.py',
  'EXPOSE 8080',
  'ENTRYPOINT ["python3", "/opt/rsctf-load/koth-service.py"]',
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
  const kothDockerfile = `${ROOT}/Dockerfile.koth`;
  writeFixture(checker, CHECKER);
  writeFixture(kothChecker, KOTH_CHECKER);
  writeFixture(service, SERVICE);
  writeFixture(kothService, KOTH_SERVICE);
  writeFixture(kothDockerfile, KOTH_DOCKERFILE);
  return {
    checker,
    kothChecker,
    service,
    kothService,
    kothDockerfile,
    root: ROOT,
  };
}
