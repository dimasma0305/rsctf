"""Build a fixed benign patch through the CLI and BuildKit on a disposable CI runner."""

import argparse
import hashlib
import io
import json
import pathlib
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid


def check(binary, directory, guest, docker_load, registry_port, stage_python=None):
    source = directory / "source"
    source.mkdir()
    (source / "Dockerfile").write_text(
        f'FROM scratch\nLABEL org.rsctf.test-run="{uuid.uuid4()}"\nCOPY service /service\nRUN ["/service"]\n'
    )
    subprocess.run(
        ["cc", "-static", "-s", "-x", "c", "-", "-o", "service"],
        cwd=source,
        input=b'#include <stdio.h>\n#include <net/if.h>\n#include <string.h>\n#include <unistd.h>\n'
        b'int main(void) { struct if_nameindex *ifs = if_nameindex(); if (!ifs) return 2; '
        b'for (struct if_nameindex *i=ifs; i->if_index; i++) if (strcmp(i->if_name,"lo")) return 3; '
        b'if (!access("/var/run/docker.sock",F_OK) || !access("/work/run/buildkit.sock",F_OK) '
        b'|| !access("/work/src/Dockerfile",F_OK)) return 4; '
        b'FILE *f = fopen("/version", "w"); if (!f) return 1; '
        b'if (fputs("patched\\n", f) < 0) return 1; return fclose(f) != 0; }\n',
        check=True,
    )
    subprocess.run(["git", "init", "-q", "--object-format=sha256"], cwd=source, check=True)
    subprocess.run(["git", "add", "Dockerfile", "service"], cwd=source, check=True)
    tree = subprocess.check_output(["git", "write-tree"], cwd=source, text=True).strip()
    archive = directory / "revision.zip"
    subprocess.run(["git", "archive", "--format=zip", "-o", archive, tree], cwd=source, check=True)

    if guest:
        return check_vm(binary, directory, guest, archive, tree, docker_load, registry_port, stage_python)

    context = directory / "context.tar"
    with context.open("wb") as output:
        subprocess.run([binary, "challenge", "patch-context", archive, tree], stdout=output, check=True)
    result = directory / "rootfs.tar"
    with context.open("rb") as stream:
        subprocess.run(
            ["docker", "buildx", "build", "--network=none", "--no-cache",
             "--output", f"type=tar,dest={result}", "-"],
            stdin=stream, timeout=120, check=True,
        )
    with tarfile.open(result) as image:
        service = image.getmember("service")
        if not service.isfile() or service.mode & 0o777 != 0o755:
            raise ValueError("service lost its executable permissions")
        if image.extractfile("version").read() != b"patched\n":
            raise ValueError("built service has the wrong version")


def vm_files(output):
    assert output.stat().st_mode & 0o777 == 0o600
    with tarfile.open(output, "r:") as image:
        def blob(descriptor):
            algorithm, key = descriptor["digest"].split(":")
            payload = image.extractfile(f"blobs/{algorithm}/{key}").read()
            assert algorithm == "sha256" and hashlib.sha256(payload).hexdigest() == key
            assert len(payload) == descriptor["size"]
            return payload

        descriptor, = json.load(image.extractfile("index.json"))["manifests"]
        manifest = json.loads(blob(descriptor))
        config = manifest["config"]
        assert json.loads(blob(config))["os"] == "linux"
        # Both readers must see exactly the same config and layer payloads.
        docker_manifest, = json.load(image.extractfile("manifest.json"))
        def blob_path(descriptor):
            return "blobs/" + descriptor["digest"].replace(":", "/")
        assert docker_manifest["Config"] == blob_path(config)
        assert docker_manifest["Layers"] == [blob_path(layer) for layer in manifest["layers"]]
        assert not docker_manifest.get("RepoTags")
        files = {}
        for layer in manifest["layers"]:
            with tarfile.open(fileobj=io.BytesIO(blob(layer))) as root:
                for entry in root:
                    if entry.isfile():
                        files[entry.name] = (entry.mode, root.extractfile(entry).read())
        assert files["service"][0] & 0o777 == 0o755
        assert files["version"][1] == b"patched\n"
        return descriptor["digest"], config["digest"], files


def check_docker_load(output, image_id):
    # This test creates an untagged image with a unique fixture label. Run the
    # already verified config ID so a mutable daemon tag cannot select the image.
    subprocess.run(["docker", "image", "load", "--input", output], check=True, timeout=60)
    try:
        inspected = json.loads(subprocess.check_output(["docker", "image", "inspect", image_id]))
        assert inspected[0]["Id"] == image_id
        assert not inspected[0].get("RepoTags")
        subprocess.run(["docker", "run", "--rm", "--pull=never", "--network=none",
                        "--cap-drop=ALL", "--security-opt=no-new-privileges", "--pids-limit=24",
                        "--memory=64m", image_id, "/service"], check=True, timeout=30)
    finally:
        subprocess.run(["docker", "image", "rm", image_id], check=True, timeout=30)


def check_registry(output, port, receipt):
    # Only the fixed test fixture is sent to this disposable loopback registry.
    ref = f"docker://127.0.0.1:{port}/rsctf/fixture@{receipt['manifestDigest']}"
    auth = output.parent / "empty-auth.json"
    auth.write_text('{"auths":{}}')
    policy = output.parent / "fixture-policy.json"
    policy.write_text(json.dumps({"default": [{"type": "reject"}], "transports": {
        "oci-archive": {str(output): [{"type": "insecureAcceptAnything"}]},
    }}))
    subprocess.run(["skopeo", "--policy", policy, "copy", "--dest-authfile", auth,
                    "--preserve-digests", "--dest-tls-verify=false", f"oci-archive:{output}", ref],
                   check=True, timeout=60)
    raw = subprocess.check_output(["skopeo", "inspect", "--authfile", auth, "--raw",
                                   "--tls-verify=false", ref], timeout=30)
    assert "sha256:" + hashlib.sha256(raw).hexdigest() == receipt["manifestDigest"]
    assert json.loads(raw)["config"]["digest"] == receipt["configDigest"]


def check_interrupted_vm(command, directory):
    before = set(directory.iterdir())
    with subprocess.Popen(command) as worker:
        try:
            deadline = time.monotonic() + 30
            emulator = None
            while emulator is None and time.monotonic() < deadline and worker.poll() is None:
                children = subprocess.run(["ps", "--ppid", str(worker.pid), "-o", "pid="],
                                          capture_output=True, text=True)
                for child in children.stdout.split():
                    process = pathlib.Path("/proc") / child
                    try:
                        arguments = (process / "cmdline").read_bytes().split(b"\0")
                    except FileNotFoundError:
                        continue
                    if b"-nodefaults" in arguments:
                        emulator = process
                        break
                time.sleep(0.05)
            assert emulator, "patch VM never started"
        finally:
            worker.kill()
            worker.wait(timeout=10)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            state = (emulator / "stat").read_text().rsplit(")", 1)[1].split()[0]
        except FileNotFoundError:
            break
        if state == "Z":
            break
        time.sleep(0.02)
    else:
        raise AssertionError("emulator survived worker death")
    assert set(directory.iterdir()) == before, "worker death left private files on disk"


def check_vm(binary, directory, guest, archive, tree, docker_load, registry_port, stage_python=None):
    output = directory / "revision.oci.tar"
    builder = pathlib.Path(__file__).parent / "ad-patch-builder" / "build.py"
    command = [sys.executable, builder, archive, tree, "--rsctf", binary, "--guest", guest, "--json", "--output", output]
    check_interrupted_vm(command, directory)
    # Admission to a particular output can race; both builds must leave exactly one candidate.
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as first:
        second = subprocess.run(command, capture_output=True, text=True)
        stdout, stderr = first.communicate(timeout=310)
    assert sorted((first.returncode, second.returncode)) == [0, 1], stderr + second.stderr
    loser = second.stderr if first.returncode == 0 else stderr
    assert "File exists" in loser or "already exists" in loser, loser
    receipt = json.loads(stdout if first.returncode == 0 else second.stdout)
    digest = "sha256:" + hashlib.sha256(output.read_bytes()).hexdigest()
    manifest, image_id, _ = vm_files(output)
    assert receipt == {"archiveDigest": digest, "manifestDigest": manifest, "configDigest": image_id}
    if docker_load:
        check_docker_load(output, image_id)
    if registry_port:
        check_registry(output, registry_port, receipt)
    if stage_python:
        subprocess.run([stage_python, builder.with_name("test_stage_registry.py"), output, manifest, image_id],
                       check=True, timeout=120)

    def repack(dockerfile):
        source = directory / "source"
        (source / "Dockerfile").write_text(dockerfile)
        subprocess.run(["git", "add", "Dockerfile"], cwd=source, check=True)
        tree = subprocess.check_output(["git", "write-tree"], cwd=source, text=True).strip()
        subprocess.run(["git", "archive", "--format=zip", "-o", archive, tree], cwd=source, check=True)
        return tree

    def rejected(args, reason):
        result = subprocess.run(args, capture_output=True, text=True)
        assert result.returncode != 0 and reason in result.stderr, result.stderr

    rejected(command, "already exists")
    other = directory / "failed.oci.tar"
    command[-1] = other
    command[3] = "0" * 64
    rejected(command, "expected Git tree")
    assert not other.exists()
    command[3] = repack('FROM scratch\nRUN ["/missing"]\n')
    rejected(command, "failed during build")
    assert not other.exists() and not list(directory.glob(".rsctf-patch-*"))
    command[3] = repack('FROM rsctf-starter\nCOPY Dockerfile /revision\nRUN ["/service"]\n')
    command += ["--base-oci", output, "--base-manifest", "sha256:" + "0" * 64]
    rejected(command, "failed during build")
    assert not other.exists()
    command[-1] = manifest
    subprocess.run(command, check=True)
    _, _, files = vm_files(other)
    assert files["revision"][1].startswith(b"FROM rsctf-starter\n")
    assert "sha256:" + hashlib.sha256(output.read_bytes()).hexdigest() == digest


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--guest", type=pathlib.Path)
    parser.add_argument("--docker-load", action="store_true", help="load and run the fixed VM-built fixture on a local Docker daemon")
    parser.add_argument("--registry-port", type=int, help="port of a disposable loopback test registry")
    parser.add_argument("--stage-python", type=pathlib.Path, help="worker Python for the PostgreSQL and TLS registry test")
    args = parser.parse_args()
    if (args.docker_load or args.registry_port is not None or args.stage_python) and not args.guest:
        parser.error("runtime image checks require --guest")
    if args.registry_port is not None and not 1 <= args.registry_port <= 65535:
        parser.error("invalid test registry port")
    with tempfile.TemporaryDirectory(prefix="rsctf-patch-build-") as temporary:
        check(args.binary.resolve(strict=True), pathlib.Path(temporary), args.guest, args.docker_load, args.registry_port, args.stage_python)
