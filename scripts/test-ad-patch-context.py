"""Build a fixed benign patch through the CLI and BuildKit on a disposable CI runner."""

import pathlib
import subprocess
import sys
import tarfile
import tempfile


def check(binary, directory):
    source = directory / "source"
    source.mkdir()
    (source / "Dockerfile").write_text(
        'FROM scratch\nCOPY service /service\nRUN ["/service"]\n'
    )
    subprocess.run(
        ["cc", "-static", "-s", "-x", "c", "-", "-o", "service"],
        cwd=source,
        input=b'#include <stdio.h>\nint main(void) {'
        b'FILE *f = fopen("/version", "w"); if (!f) return 1; '
        b'if (fputs("patched\\n", f) < 0) return 1; return fclose(f) != 0; }\n',
        check=True,
    )
    subprocess.run(["git", "init", "-q", "--object-format=sha256"], cwd=source, check=True)
    subprocess.run(["git", "add", "Dockerfile", "service"], cwd=source, check=True)
    tree = subprocess.check_output(["git", "write-tree"], cwd=source, text=True).strip()
    archive = directory / "revision.zip"
    subprocess.run(["git", "archive", "--format=zip", "-o", archive, tree], cwd=source, check=True)

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


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="rsctf-patch-build-") as temporary:
        check(pathlib.Path(sys.argv[1]).resolve(strict=True), pathlib.Path(temporary))
