#!/usr/bin/env python3
"""Exercise the shipped callback proxy across a real Docker INPUT fence.

Requires Docker with NET_ADMIN support. The upstream is an
HTTP fixture: this checks routing/header/body preservation, not RSCTF scoring.
"""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import uuid

ROOT = Path(__file__).resolve().parents[1]
PYTHON_IMAGE = "python@sha256:7984cf56ceaddebe4fefeb75d64000b9f45d8b77b11a09b24524da6cdc3af0c8"

SERVER = r'''
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/healthz":
            self.send_response(200); self.end_headers(); self.wfile.write(b"ok"); return
        body = self.rfile.read(int(self.headers.get("Content-Length", 0))).decode()
        status = 200 if self.headers.get("Authorization") == "Bearer fixture" else 401
        self.send_response(status); self.end_headers()
        self.wfile.write(json.dumps({"path": self.path, "method": self.command,
            "body": body, "signature": self.headers.get("X-Koth-Signature")}).encode())
    do_POST = do_GET
    def log_message(self, *args): pass
ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
'''

CLIENT = r'''
import json, socket, sys, urllib.error, urllib.request
mode, owner = sys.argv[1:]
def request(origin, path, method="GET", body=None, authenticated=True):
    headers = {"X-Koth-Signature": "signed-fixture"}
    if authenticated: headers["Authorization"] = "Bearer fixture"
    req = urllib.request.Request(origin + path, data=body, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=2) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.read()
direct = "http://" + owner + ":8080"
if mode == "before":
    assert request(direct, "/healthz") == (200, b"ok")
    print("direct callback positive control passed")
    sys.exit(0)
try:
    request(direct, "/healthz")
except (urllib.error.URLError, TimeoutError, socket.timeout):
    pass
else:
    raise AssertionError("direct callback bypassed the INPUT fence")
proxy = "http://rsctf-koth-reporter:8080"
assert request(proxy, "/healthz") == (200, b"ok")
context = "/api/v1/koth/games/46/challenges/839/context"
observation = "/api/v1/koth/games/46/challenges/839/observations"
for method, path, body in [("GET", context, None), ("POST", observation, b'{"sequence":1}')]:
    code, raw = request(proxy, path, method, body)
    assert code == 200, (code, raw)
    assert json.loads(raw) == {"method": method, "path": path,
        "body": body.decode() if body else "", "signature": "signed-fixture"}
    assert request(proxy, path, method, body, authenticated=False)[0] == 401
for method, path in [("POST", context), ("GET", observation),
                     ("GET", "/api/admin/users"), ("GET", "/api/account/profile"),
                     ("GET", "/metrics"), ("GET", context + "/extra"),
                     ("GET", "/api/v1/koth/games/46/challenges/839/status")]:
    assert request(proxy, path, method)[0] == 404, (method, path)
print("callback GET/POST, credentials, signatures, body, route denial and direct fence passed")
'''


def run(*args, input=None, env=None, check=True):
    result = subprocess.run(args, input=input, text=True, capture_output=True,
                            env=env, cwd=ROOT, timeout=180)
    if check and result.returncode:
        raise RuntimeError(f"{args[0]} failed: {result.stderr or result.stdout}")
    return result.stdout


def verify(split):
    owner = "rsctf-control" if split else "rsctf"
    project = "rsctf-reporter-test-" + uuid.uuid4().hex[:10]
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("RSCTF_", "POSTGRES_", "COMPOSE_"))}
    env.update(POSTGRES_PASSWORD="fixture", RSCTF_JWT_SECRET="fixture",
               RSCTF_IDENTITY_HASH_KEY="fixture", RSCTF_PUBLIC_URL="http://localhost",
               RSCTF_DOCKER_PUBLIC_ENTRY="fixture.invalid",
               RSCTF_AD_VPN_SERVER_ENDPOINT="fixture.invalid:51820",
               RSCTF_IMAGE="example.invalid/unused:fixture", COMPOSE_PROJECT_NAME=project)
    files = ["deploy/compose.yml"]
    if split:
        files += ["deploy/compose.roles.yml", "deploy/compose.roles.ad-vpn.yml"]
    else:
        files += ["deploy/compose.ad-vpn.yml"]
    command = ["docker", "compose", "--env-file", "/dev/null", "-p", project]
    for file in files:
        command += ["-f", file]
    rendered = json.loads(run(*command, "config", "--format", "json", env=env))
    # Exercise the actual rendered proxy without starting production DB/app roles.
    proxy = rendered["services"]["koth-reporter"]
    proxy["restart"] = "no"
    health = ["CMD", "python", "-c",
              "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/healthz')"]
    fixture = {"image": PYTHON_IMAGE, "command": ["python", "-c", SERVER],
               "environment": {"NO_PROXY": "*", "no_proxy": "*"},
               "networks": {"default": {"aliases": ["rsctf-network"]}, "rsctf-ad": {}},
               "healthcheck": {"test": health, "interval": "1s", "timeout": "2s", "retries": 30}}
    client = {"image": PYTHON_IMAGE, "command": ["sleep", "180"], "networks": ["rsctf-ad"],
              "environment": {"NO_PROXY": "*", "no_proxy": "*"}}
    document = {"services": {owner: fixture, "koth-reporter": proxy, "client": client},
                "networks": {key: rendered["networks"][key] for key in ("default", "rsctf-ad")}}
    with tempfile.TemporaryDirectory(prefix="rsctf-reporter-") as directory:
        path = Path(directory) / "compose.json"
        path.write_text(json.dumps(document))
        compose = ["docker", "compose", "-p", project, "-f", str(path)]
        try:
            run(*compose, "up", "-d", "--wait", "--wait-timeout", "60", env=env)
            print(run(*compose, "exec", "-T", "client", "python", "-", "before", owner,
                      input=CLIENT, env=env).strip(), flush=True)
            container = run(*compose, "ps", "-q", owner, env=env).strip()
            subnet = document["networks"]["rsctf-ad"]["ipam"]["config"][0]["subnet"]
            # The production VPN fence denies new service-network INPUT traffic.
            # This fixture installs that same denial without modifying host rules.
            run("docker", "run", "--rm", "--network", "container:" + container,
                "--cap-add", "NET_ADMIN",
                "-v", "/etc/ssl/certs/ca-certificates.crt:/etc/ssl/cert.pem:ro",
                PYTHON_IMAGE, "sh", "-ec",
                'apk add --no-cache iptables >/dev/null; exec iptables "$@"', "--", "-I", "INPUT", "1",
                "-s", subnet, "-p", "tcp", "--dport", "8080", "-j", "DROP")
            print(run(*compose, "exec", "-T", "client", "python", "-", "after", owner,
                      input=CLIENT, env=env).strip(), flush=True)
            print(f"{owner}: shipped reporter proxy passed", flush=True)
        except Exception:
            print(run(*compose, "logs", "--no-color", "--tail", "30", env=env, check=False), flush=True)
            proxy_id = run(*compose, "ps", "-aq", "koth-reporter", env=env).strip()
            if proxy_id:
                print(run("docker", "inspect", "-f", "{{json .State.Health}}", proxy_id), flush=True)
            raise
        finally:
            run(*compose, "down", "--volumes", "--remove-orphans", env=env)


if __name__ == "__main__":
    verify(False)
    verify(True)
