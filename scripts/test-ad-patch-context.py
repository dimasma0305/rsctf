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


def check(binary, directory, guest):
    source = directory / "source"
    source.mkdir()
    (source / "Dockerfile").write_text(
        'FROM scratch\nCOPY service /service\nRUN ["/service"]\n'
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
        return check_vm(binary, directory, guest, archive, tree)

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
        assert json.loads(blob(manifest["config"]))["os"] == "linux"
        files = {}
        for layer in manifest["layers"]:
            with tarfile.open(fileobj=io.BytesIO(blob(layer))) as root:
                for entry in root:
                    if entry.isfile():
                        files[entry.name] = (entry.mode, root.extractfile(entry).read())
        assert files["service"][0] & 0o777 == 0o755
        assert files["version"][1] == b"patched\n"
        return descriptor["digest"], files


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


def check_vm(binary, directory, guest, archive, tree):
    output = directory / "revision.oci.tar"
    builder = pathlib.Path(__file__).parent / "ad-patch-builder" / "build.py"
    command = [sys.executable, builder, archive, tree, "--rsctf", binary, "--guest", guest, "--output", output]
    check_interrupted_vm(command, directory)
    # Admission to a particular output can race; both builds must leave exactly one candidate.
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as first:
        second = subprocess.run(command, capture_output=True, text=True)
        stdout, stderr = first.communicate(timeout=310)
    assert sorted((first.returncode, second.returncode)) == [0, 1], stderr + second.stderr
    loser = second.stderr if first.returncode == 0 else stderr
    assert "File exists" in loser or "already exists" in loser, loser
    digest = (stdout if first.returncode == 0 else second.stdout).strip()
    assert digest == "sha256:" + hashlib.sha256(output.read_bytes()).hexdigest()
    manifest, _ = vm_files(output)

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
    _, files = vm_files(other)
    assert files["revision"][1].startswith(b"FROM rsctf-starter\n")
    assert "sha256:" + hashlib.sha256(output.read_bytes()).hexdigest() == digest


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=pathlib.Path)
    parser.add_argument("--guest", type=pathlib.Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="rsctf-patch-build-") as temporary:
        check(args.binary.resolve(strict=True), pathlib.Path(temporary), args.guest)
