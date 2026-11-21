"""Transfer the fixed VM-built CI image through PostgreSQL to a loopback TLS registry."""

import hashlib
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import sys
import time
from unittest.mock import patch
from urllib.request import urlopen

from stage import stage_candidate
from test_export import CandidateFixture


def check(archive, manifest, config, docker_pull=False):
    fixture = CandidateFixture()
    try:
        fixture.setUp()
        db, root = fixture.db, fixture.path.parent
        data = archive.read_bytes()
        db.execute('UPDATE "AdPatchCandidates" SET archive_bytes=%s,digest=%s,manifest_digest=%s,config_digest=%s',
                   (len(data), hashlib.sha256(data).hexdigest(), manifest, config))
        db.execute('DELETE FROM "AdPatchCandidateChunks"')
        chunk = 1024 * 1024
        for start in range(0, len(data), chunk):
            db.execute('INSERT INTO "AdPatchCandidateChunks" VALUES (1,%s,%s)',
                       (start // chunk, data[start:start+chunk]))

        trust = root / "trust"
        trust.mkdir()
        cert, key = trust / "ca.crt", root / "key.pem"
        subprocess.run(["openssl", "req", "-x509", "-nodes", "-newkey", "rsa:2048", "-days", "1",
                        "-subj", "/CN=localhost", "-addext", "subjectAltName=IP:127.0.0.1",
                        "-keyout", key, "-out", cert], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            host = f"127.0.0.1:{sock.getsockname()[1]}"
        registry = root / "registry.yml"
        registry.write_text(f"version: 0.1\nlog:\n  level: error\nstorage:\n  filesystem:\n"
                            f"    rootdirectory: {root}/blobs\nhttp:\n  addr: {host}\n"
                            f"  tls:\n    certificate: {cert}\n    key: {key}\n")
        policy = root / ".config" / "containers"
        policy.mkdir(parents=True)
        (policy / "policy.json").write_text(json.dumps({"default": [{"type": "reject"}], "transports": {
            "oci-archive": {"": [{"type": "insecureAcceptAnything"}]}}}))
        auth = root / "empty-auth.json"
        auth.write_text('{"auths":{}}')
        with subprocess.Popen(["docker-registry", "serve", registry], stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL) as proc:
            try:
                context = ssl.create_default_context(cafile=str(cert))
                for _ in range(100):
                    try:
                        with urlopen(f"https://{host}/v2/", context=context, timeout=1) as response:
                            assert response.status == 200
                        break
                    except OSError:
                        if proc.poll() is not None:
                            raise RuntimeError("loopback registry exited during startup")
                        time.sleep(.1)
                else:
                    raise RuntimeError("loopback TLS registry did not start")

                args = (db, 1, 4, fixture.attempt, host + "/rsctf", auth)
                with patch.dict(os.environ, HOME=str(root), XDG_CONFIG_HOME=str(policy.parent)):
                    with fixture.assertRaises(subprocess.CalledProcessError):
                        stage_candidate(*args)
                    assert db.execute('SELECT registry_image_ref FROM "AdPatchCandidates"').fetchone() == (None,)
                    schema = db.execute("SELECT current_schema()").fetchone()[0]
                    env = {**os.environ, "RSCTF_DATABASE_URL": os.environ["RSCTF_TEST_DATABASE_URL"],
                           "PGOPTIONS": f"-c search_path={schema}"}
                    command = [sys.executable, str(Path(__file__).with_name("stage.py")),
                               "--watch-challenge", "11", "--repository", host + "/rsctf",
                               "--cert-dir", str(trust), "--authfile", str(auth)]
                    watcher = subprocess.Popen(command, env=env)
                    try:
                        deadline = time.monotonic() + 60
                        while time.monotonic() < deadline:
                            ref = db.execute('SELECT registry_image_ref FROM "AdPatchCandidates"').fetchone()[0]
                            if ref:
                                break
                            assert watcher.poll() is None, "registry watcher exited before staging"
                            time.sleep(.2)
                        duplicate = subprocess.run(command, env=env, capture_output=True, timeout=5)
                        assert duplicate.returncode and b"another registry watcher" in duplicate.stderr
                    finally:
                        watcher.terminate()
                        watcher.wait(timeout=5)
                    assert ref == host + "/rsctf/service-1@" + manifest
                    assert stage_candidate(*args, trust) == ref
                    assert db.execute('SELECT registry_image_ref FROM "AdPatchCandidates"').fetchone() == (ref,)
                if docker_pull:
                    # The CI registry and Docker daemon share this loopback address.
                    # Give Docker an empty client config, independent of runner credentials.
                    client = root / "docker-client"
                    client.mkdir()
                    docker = ["docker", "--config", str(client)]
                    try:
                        subprocess.run([*docker, "pull", ref], check=True, timeout=60)
                        info, = json.loads(subprocess.check_output([*docker, "image", "inspect", ref], timeout=15))
                        assert info["Id"] == config and ref in info["RepoDigests"]
                        subprocess.run([*docker, "run", "--rm", "--pull=never", "--network=none",
                                        "--cap-drop=ALL", "--security-opt=no-new-privileges", "--pids-limit=24",
                                        "--memory=64m", ref, "/service"], check=True, timeout=30)
                    finally:
                        subprocess.run([*docker, "image", "rm", ref], capture_output=True, timeout=30)
            finally:
                proc.terminate()
                proc.wait(timeout=10)
    finally:
        fixture.doCleanups()
    print("Verified the private candidate receipt after TLS registry transfer and retry.")


if __name__ == "__main__":
    check(Path(sys.argv[1]), sys.argv[2], sys.argv[3], "--docker-pull" in sys.argv[4:])
