#!/usr/bin/env python3
"""Exercise installer decisions and piped setup without a Docker daemon."""

import errno
import hashlib
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent
IMAGE = "ghcr.io/dimasma0305/rsctf@sha256:" + "0" * 64


class InstallerUX(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="rsctf-install-ux-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.checkout = self.root / "rsctf"
        shutil.copytree(ROOT / "deploy", self.checkout / "deploy", ignore=shutil.ignore_patterns(".env"))
        (self.checkout / "scripts").mkdir()
        self.installer = self.checkout / "scripts/install.sh"
        shutil.copy2(ROOT / "scripts/install.sh", self.installer)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        docker = self.bin / "docker"
        docker.write_text("""#!/bin/sh
printf '%s\\n' "$*" >> "$DOCKER_TEST_LOG"
case "$*" in
  'compose version') exit "${COMPOSE_TEST_STATUS:-0}" ;;
  info) exit "${DAEMON_TEST_STATUS:-0}" ;;
  'compose config --services') printf 'db\\nredis\\nrsctf\\n' ;;
esac
""")
        docker.chmod(0o755)
        self.log = self.root / "docker.log"
        self.env = {**os.environ, "PATH": f"{self.bin}:/usr/bin:/bin", "DOCKER_TEST_LOG": str(self.log)}
        for key in ("RSCTF_IMAGE", "RSCTF_REF", "RSCTF_INSTALL_DIR", "RSCTF_BUNDLE_URL"):
            self.env.pop(key, None)
        self.environment = self.checkout / "deploy/.env"

    def run_installer(self, *args):
        return subprocess.run(
            ["bash", str(self.installer), "--image", IMAGE, *args],
            cwd=self.checkout, env=self.env, text=True, capture_output=True, timeout=10,
        )

    def terminal(self, args, answers=""):
        pid, master = pty.fork()
        if pid == 0:
            os.chdir(self.checkout)
            os.execvpe("bash", ["bash", *args], self.env)
        output = bytearray()
        try:
            if answers:
                os.write(master, answers.encode())
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        break
                    if not chunk:
                        break
                    output.extend(chunk)
                finished, status = os.waitpid(pid, os.WNOHANG)
                if finished:
                    pid = None
                    return os.waitstatus_to_exitcode(status), output.decode()
            else:
                self.fail("installer asked an unexpected question or did not finish")
            _, status = os.waitpid(pid, 0)
            pid = None
            return os.waitstatus_to_exitcode(status), output.decode()
        finally:
            os.close(master)
            if pid:
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)

    def test_basic_local_needs_only_one_answer(self):
        status, output = self.terminal([str(self.installer), "--image", IMAGE, "--configure-only"], "1\n")
        self.assertEqual(status, 0, output)
        text = self.environment.read_text()
        self.assertIn("RSCTF_PUBLIC_URL=http://localhost:8080\n", text)
        self.assertIn("COMPOSE_FILE=compose.yml\n", text)
        self.assertNotIn("Enable Docker-backed", output)
        self.assertNotIn("Local HTTP port [", output)
        self.assertIn("RSCTF_BOOTSTRAP_TOKEN=\n", text)
        self.assertIn("RSCTF_BOOTSTRAP_AUTO=true\n", text)
        self.assertNotIn("Setup token:", output)
        self.assertIn("first-start Docker logs", output)

    def test_piped_script_can_read_wizard_answers_from_the_terminal(self):
        fixture = self.root / "release"
        fixture.mkdir()
        for helper in ("compose-maintenance-cutover.sh", "kubernetes-maintenance-cutover.sh", "docker-proxy-firewall.sh"):
            shutil.copy2(ROOT / "scripts" / helper, self.checkout / "scripts" / helper)
        (self.checkout / "deploy/release.env").write_text(
            f"RSCTF_RELEASE_VERSION=v1.2.3\nRSCTF_RELEASE_IMAGE={IMAGE}\n"
        )
        archive = fixture / "rsctf-deployment-bundle.tar.gz"
        subprocess.run(["tar", "-czf", str(archive), "-C", str(self.root), "rsctf"], check=True)
        (fixture / "SHA256SUMS").write_text(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
        (fixture / "rsctf-worker-agent-attestation.json").write_text("{}\n")
        for command in ("curl", "gh"):
            (self.bin / command).symlink_to(ROOT / "scripts/test-install-shim.sh")
        self.env.update({
            "RSCTF_INSTALLER_FIXTURE": str(fixture),
            "RSCTF_TEST_CURL_LOG": str(self.root / "curl.log"),
            "RSCTF_TEST_GH_LOG": str(self.root / "gh.log"),
        })
        target = self.root / "installed"
        status, output = self.terminal([
            "-c", 'cat "$1" | bash -s -- --install-dir "$2" --configure-only',
            "rsctf-pipe-test", str(self.installer), str(target),
        ], "1\n")
        self.assertEqual(status, 0, output)
        self.assertTrue((target / "deploy/.env").is_file())
        self.assertFalse(self.environment.exists(), "piped code must not use the working checkout")
        self.assertIn("--source-ref refs/tags/v1.2.3", (self.root / "gh.log").read_text())

    def test_public_server_needs_only_choice_and_domain(self):
        status, output = self.terminal(
            [str(self.installer), "--image", IMAGE, "--configure-only"], "2\nctf.example.test\n",
        )
        self.assertEqual(status, 0, output)
        text = self.environment.read_text()
        self.assertIn("RSCTF_PUBLIC_URL=https://ctf.example.test\n", text)
        self.assertIn("COMPOSE_FILE=compose.yml:compose.caddy.yml\n", text)

    def test_advanced_proxy_choices_remain_available(self):
        status, output = self.terminal(
            [str(self.installer), "--image", IMAGE, "--configure-only"],
            "3\n3\nhttps://ctf.example.test\n127.0.0.1/32\nn\n",
        )
        self.assertEqual(status, 0, output)
        text = self.environment.read_text()
        self.assertIn("RSCTF_TRUSTED_PROXY_CIDRS=127.0.0.1/32\n", text)
        self.assertIn("COMPOSE_FILE=compose.yml\n", text)

    def test_unavailable_daemon_fails_before_configuration(self):
        self.env["DAEMON_TEST_STATUS"] = "1"
        result = self.run_installer("--mode", "local", "--non-interactive")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("docker info", result.stderr)
        self.assertFalse(self.environment.exists())

    def test_missing_compose_fails_before_configuration(self):
        self.env["COMPOSE_TEST_STATUS"] = "1"
        result = self.run_installer("--configure-only", "--non-interactive")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Compose v2", result.stderr)
        self.assertFalse(self.environment.exists())

    def test_vpn_configuration_needs_no_daemon_or_host_devices(self):
        self.env["DAEMON_TEST_STATUS"] = "1"
        uname = self.bin / "uname"
        uname.write_text("#!/bin/sh\nprintf 'Darwin\\n'\n")
        uname.chmod(0o755)
        result = self.run_installer(
            "--mode", "caddy", "--domain", "ctf.example.test", "--with-ad-vpn",
            "--configure-only", "--non-interactive",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("compose.ad-vpn.yml", self.environment.read_text())
        self.assertNotIn("compose up", self.log.read_text())

    def test_unsupported_runtime_fails_before_configuration(self):
        uname = self.bin / "uname"
        uname.write_text("#!/bin/sh\nprintf 'Darwin\\n'\n")
        uname.chmod(0o755)
        result = self.run_installer(
            "--mode", "caddy", "--domain", "ctf.example.test", "--with-docker", "--non-interactive",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Linux host-firewall", result.stderr)
        self.assertFalse(self.environment.exists())


if __name__ == "__main__":
    unittest.main()
