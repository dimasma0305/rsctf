"""Build a saved defensive patch on a disposable, offline Linux x86-64 guest."""

import argparse
from contextlib import ExitStack, contextmanager
import ctypes
import errno
import hashlib
import os
from pathlib import Path
import re
import resource
import signal
import stat
import subprocess
import sys
import tempfile


# Sector-aligned header followed by an opaque OCI archive. Never mount a guest filesystem.
HEADER_BYTES = 65536
MAX_IMAGE_BYTES = 256 * 1024 * 1024
DEADLINE_SECONDS = 300


def copy_result(source, output):
    source.seek(0)
    header = source.read(HEADER_BYTES)
    failure = re.fullmatch(rb"RSCTF-PATCH-FAILED (startup|context|base|daemon|build|export)\n\x00*", header)
    if failure:
        raise ValueError("patch VM failed during " + failure[1].decode())
    receipt = re.fullmatch(rb"RSCTF-PATCH-OCI 1 ([1-9][0-9]{0,8}) ([a-f0-9]{64})\n\x00*", header)
    if not receipt:
        raise ValueError("patch VM did not finish the OCI export")
    remaining = int(receipt[1])
    if remaining > MAX_IMAGE_BYTES:
        raise ValueError("patch image exceeds the worker output limit")
    digest = hashlib.sha256()
    while remaining:
        data = source.read(min(remaining, 1024 * 1024))
        if not data:
            raise ValueError("truncated patch image")
        output.write(data)
        digest.update(data)
        remaining -= len(data)
    if digest.hexdigest().encode() != receipt[2]:
        raise ValueError("patch image transfer digest mismatch")
    return digest.hexdigest()


def constrain_emulator(parent):
    # Linux PR_SET_PDEATHSIG. Check the race where the parent exited first.
    if ctypes.CDLL(None).prctl(1, signal.SIGKILL, 0, 0, 0) != 0 or os.getppid() != parent:
        os._exit(1)
    limit = DEADLINE_SECONDS * 2
    _, hard = resource.getrlimit(resource.RLIMIT_CPU)
    if hard != resource.RLIM_INFINITY:
        limit = min(limit, hard)
    resource.setrlimit(resource.RLIMIT_CPU, (limit, limit))


def build_to(args, scratch, candidate, source=None):
    if os.getuid() == 0:
        raise ValueError("run the patch VM as an unprivileged worker account")
    kernel = (args.guest / "vmlinuz").resolve(strict=True)
    initrd = (args.guest / "initramfs.cpio").resolve(strict=True)
    if bool(args.base_oci) != bool(args.base_manifest):
        raise ValueError("the starter image needs both --base-oci and --base-manifest")
    if args.base_manifest and not re.fullmatch(r"sha256:[a-f0-9]{64}", args.base_manifest):
        raise ValueError("invalid starter image manifest digest")
    with ExitStack() as stack:
        # Anonymous spools disappear on cancellation or worker death, even
        # when the emulator has produced hundreds of MiB of private bytes.
        out = stack.enter_context(tempfile.TemporaryFile(dir=scratch))
        subprocess.run([args.rsctf, "challenge", "patch-context", args.zip, args.tree],
                       stdin=source, stdout=out, check=True, timeout=DEADLINE_SECONDS)
        context = stack.enter_context(open(f"/proc/self/fd/{out.fileno()}", "rb"))
        disk = stack.enter_context(tempfile.TemporaryFile(dir=scratch))
        disk.truncate(HEADER_BYTES + MAX_IMAGE_BYTES)
        # QEMU probes a raw device read-only before opening the writable backend.
        disk_read = stack.enter_context(open(f"/proc/self/fd/{disk.fileno()}", "rb"))
        base_drive = []
        inherited = [context.fileno(), disk.fileno(), disk_read.fileno()]
        boot_args = "panic=1 rdinit=/init"
        if args.base_oci:
            base = stack.enter_context(args.base_oci.open("rb"))
            metadata = os.fstat(base.fileno())
            if not stat.S_ISREG(metadata.st_mode) or not 0 < metadata.st_size <= MAX_IMAGE_BYTES:
                raise ValueError("invalid starter OCI archive size")
            inherited.append(base.fileno())
            base_drive = [
                "-add-fd", f"fd={base.fileno()},set=3",
                "-drive", "file=/dev/fdset/3,format=raw,if=none,id=starter,readonly=on",
                "-device", "virtio-blk-pci,drive=starter,serial=rsctf-starter",
            ]
            boot_args += " rsctf_base=" + args.base_manifest
        accelerator = "kvm" if args.kvm else "tcg"
        parent = os.getpid()
        emulator_env = {key: os.environ[key] for key in ("PATH", "QEMU_MODULE_DIR") if key in os.environ}
        subprocess.run([
            "qemu-system-x86_64", "-no-user-config", "-nodefaults", "-machine", f"q35,accel={accelerator}",
            "-cpu", "max", "-smp", "2", "-m", "1536", "-nic", "none", "-display", "none",
            "-monitor", "none", "-serial", "none", "-no-reboot",
            "-sandbox", "on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny",
            "-kernel", str(kernel), "-initrd", str(initrd), "-append", boot_args,
            "-add-fd", f"fd={context.fileno()},set=1",
            "-drive", "file=/dev/fdset/1,format=raw,if=none,id=context,readonly=on",
            "-device", "virtio-blk-pci,drive=context,serial=rsctf-context",
            "-add-fd", f"fd={disk.fileno()},set=2",
            "-add-fd", f"fd={disk_read.fileno()},set=2",
            "-drive", "file=/dev/fdset/2,format=raw,if=none,id=result",
            "-device", "virtio-blk-pci,drive=result,serial=rsctf-result",
            *base_drive,
        ], pass_fds=inherited, stdin=subprocess.DEVNULL, env=emulator_env, check=True, timeout=DEADLINE_SECONDS,
            preexec_fn=lambda: constrain_emulator(parent))
        return copy_result(disk, candidate)


@contextmanager
def private_archive(path):
    """Publish a complete private archive once, even with competing writers."""
    if path.exists():
        raise FileExistsError("patch output already exists")
    with ExitStack() as stack:
        directory = os.open(path.parent, os.O_DIRECTORY)
        stack.callback(os.close, directory)
        try:
            fd = os.open(".", os.O_TMPFILE | os.O_WRONLY, 0o600, dir_fd=directory)
        except OSError as error:
            if error.errno in (errno.EOPNOTSUPP, errno.EINVAL, errno.EISDIR):
                raise ValueError("use a local worker output directory supporting O_TMPFILE") from error
            raise
        candidate = stack.enter_context(os.fdopen(fd, "wb"))
        yield candidate
        candidate.flush()
        os.fsync(candidate.fileno())
        # Link the verified inode exactly once. A concurrent winner is preserved.
        os.link(f"/proc/self/fd/{candidate.fileno()}", path.name, dst_dir_fd=directory)
        os.fsync(directory)


def build(args):
    with private_archive(args.output) as candidate:
        return build_to(args, args.output.parent, candidate)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("zip", type=Path)
    parser.add_argument("tree", help="full Git tree ID from the saved revision")
    parser.add_argument("--guest", required=True, type=Path, help="trusted kernel and initramfs directory")
    parser.add_argument("--output", required=True, type=Path, help="new private OCI archive")
    parser.add_argument("--rsctf", default="rsctf", help="rsctf CLI executable")
    parser.add_argument("--kvm", action="store_true", help="use an available KVM device")
    parser.add_argument("--base-oci", type=Path, help="organizer-approved OCI tar for FROM rsctf-starter")
    parser.add_argument("--base-manifest", help="approved sha256 manifest digest inside the starter archive")
    try:
        digest = build(parser.parse_args())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
    print("sha256:" + digest)


if __name__ == "__main__":
    main()
