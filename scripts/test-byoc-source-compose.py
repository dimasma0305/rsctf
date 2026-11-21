"""Run the downloaded BYOC bundle with a real Docker daemon and the current agent."""

import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import tempfile
import time


def docker(*args, cwd=None):
    return subprocess.check_output(["docker", *args], cwd=cwd, text=True, timeout=120).strip()


def agent_image(stack, base, tag, binary):
    container = docker("create", base, "sleep", "300")
    stack.callback(docker, "rm", "-v", container)
    docker("cp", binary, f"{container}:/rsctf-byoc-agent")
    docker("commit", "--change", 'ENTRYPOINT ["/rsctf-byoc-agent"]', container, tag)
    stack.callback(docker, "image", "rm", tag)


def wait_for_report(container, *expected):
    deadline = time.monotonic() + 90
    while True:
        log = subprocess.check_output(["docker", "logs", "--tail", "100", container],
                                      stderr=subprocess.STDOUT, text=True, timeout=10)
        log = re.sub(r"\x1b\[[0-9;]*m", "", log)
        if any(all(word in line for word in expected) for line in log.splitlines()):
            return
        if time.monotonic() >= deadline or docker("inspect", "-f", "{{.State.Running}}", container) != "true":
            raise RuntimeError(f"missing agent report {expected}:\n{log}")
        time.sleep(1)


def check(args, root, stack):
    game = secrets.randbelow(2**31 - 1) + 1
    tag = f"rsctf-source-test-{game}"
    project = f"rsctf-byoc-{game}-1"
    example = (Path(__file__).resolve().parents[1] / "examples/challenge-repository"
               / "challenges/AD/Web/self-hosted-service/src")
    context = root / "example"
    shutil.copytree(example, context)
    # A private checkout can have owner-only source. Exercise the image's
    # installed permissions independently of the CI runner's umask.
    (context / "app.py").chmod(0o600)
    docker("build", "--network=none", "-t", f"{tag}:service", context)
    stack.callback(docker, "image", "rm", f"{tag}:service")
    service = docker("image", "inspect", "--format", "{{.Id}}", f"{tag}:service")
    agent_image(stack, args.agent_base, f"{tag}:agent", args.agent)
    docker("image", "save", "-o", root / "service.tar", f"{tag}:service")
    alias = f"{project}-service:reviewed-{hashlib.sha256(service.encode()).hexdigest()[:16]}"
    compose_file = root / project / "docker-compose.yml"
    compose = ["compose", "--env-file", "/dev/null", "-f", compose_file]

    def clean_bundle():
        peer = root / "other-team" / "compose.yaml"
        if peer.exists():
            docker("compose", "-f", peer, "down", "-v", "--timeout", "1")
        if compose_file.exists():
            docker(*compose, "down", "-v", "--timeout", "1")
        # A regression may replace the Compose file, orphaning the original volume.
        for volume in docker("volume", "ls", "-q", "--filter",
                             f"label=com.docker.compose.project={project}").splitlines():
            docker("volume", "rm", volume)
        if subprocess.run(["docker", "image", "inspect", alias], stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL).returncode == 0:
            docker("image", "rm", alias)

    stack.callback(clean_bundle)
    peer_source = root / "other-team" / "service-src"
    peer_source.mkdir(parents=True)
    peer_source.chmod(0o755)
    (peer_source / "host.py").write_text("print('original')\n")
    (peer_source / "host.py").chmod(0o644)
    bridge = json.loads(docker("network", "inspect", "bridge"))[0]["IPAM"]["Config"]
    ipv4 = next(net for net in bridge if ":" not in net["Subnet"])
    env = {**os.environ, "RSCTF_AD_VPN_ENABLED": "false", "RSCTF_AD_VPN_SERVICES_CIDR": ipv4["Subnet"],
           "RSCTF_TEST_BYOC_BIND": ipv4["Gateway"], "RSCTF_TEST_BYOC_GAME": str(game), "RSCTF_TEST_BYOC_IMAGE_ID": service,
           "RSCTF_TEST_BYOC_BUNDLE": str(root)}
    subprocess.run(args.test_command, check=True, env=env, timeout=600)
    standalone = json.loads(docker("compose", "-f", root / "standalone.yml", "config", "--format", "json"))
    mounts = standalone["services"]["rsctf-agent"]["volumes"]
    assert any(v["source"] == str(root / "service-src") and v["target"] == "/rsctf-source"
               and v["read_only"] for v in mounts)

    agent = docker(*compose, "ps", "-q", "rsctf-agent")
    mounts = json.loads(docker("inspect", "-f", "{{json .Mounts}}", agent))
    assert any(v["Destination"] == "/rsctf-source" and not v["RW"] for v in mounts)
    assert all(v["Destination"] != "/var/run/docker.sock" for v in mounts)
    wait_for_report(agent, "source baseline recorded")
    wait_for_report(agent, "source changed", "app.py", "modified")
    # A network/export failure on reinstall must leave both the team's files
    # and the existing service identity intact.
    config = compose_file.read_bytes()
    vpn = compose_file.with_name("rsctf-ad.conf")
    vpn.write_text("# team-local test settings\n")
    before = docker(*compose, "ps", "-q", "service")
    # A healthy export must also preserve team-owned changes to the definition.
    custom = config.replace(b"restart: unless-stopped", b"restart: always", 1)
    assert custom != config
    compose_file.write_bytes(custom)
    rerun = subprocess.run(["sh", "setup.sh"], cwd=root, capture_output=True, text=True, timeout=60)
    assert rerun.returncode != 0 and "Kept your existing BYOC setup" in rerun.stderr
    assert compose_file.read_bytes() == custom and vpn.read_text() == "# team-local test settings\n"
    assert docker(*compose, "ps", "-q", "service") == before
    assert docker(*compose, "ps", "-q", "rsctf-agent") == agent
    docker(*compose, "down", "--timeout", "1")
    rerun = subprocess.run(["sh", "setup.sh"], cwd=root, capture_output=True, text=True, timeout=60)
    assert rerun.returncode != 0 and "Kept your existing BYOC setup" in rerun.stderr
    assert compose_file.read_bytes() == custom and vpn.read_text() == "# team-local test settings\n"
    assert not docker(*compose, "ps", "-aq")
    compose_file.write_bytes(config)
    subprocess.run(["sh", "setup.sh"], cwd=root, check=True, timeout=60)
    before = docker(*compose, "ps", "-q", "service")
    assert before
    docker(*compose, "exec", "-T", "service", "grep", "-Fxq", "# team rehearsal patch", "/app/app.py")
    assert compose_file.read_bytes() == config
    print("A healthy export preserved customized running and stopped setups; an unmodified reinstall reused the patched source.")
    (root / "service.tar").unlink()
    vpn.write_text("# team-local test settings\n")
    for installer in ("setup.sh", "withdrawn.sh"):
        rerun = subprocess.run(["sh", installer], cwd=root, capture_output=True, text=True, timeout=30)
        assert rerun.returncode != 0 and "Kept your existing BYOC setup" in rerun.stderr
        assert compose_file.read_bytes() == config and vpn.read_text() == "# team-local test settings\n"
        assert docker(*compose, "ps", "-q", "service") == before
    docker(*compose, "up", "-d", "--wait", "--wait-timeout", "45", "--force-recreate", "service")
    docker(*compose, "exec", "-T", "service", "grep", "-Fxq", "# team rehearsal patch", "/app/app.py")
    assert docker(*compose, "exec", "-T", "service", "wget", "-Y", "off", "-qO-",
                  "http://127.0.0.1:8080/health") == "ok"
    print("The BYOC example retained the team's app.py edit and answered /health after recreation.")

    # A separate participant takes the Compose download, with no setup script,
    # manifest, or command-line flags. Use the filename suggested by the UI.
    docker(*compose, "down", "-v", "--timeout", "1")
    rerun = subprocess.run(["sh", "withdrawn.sh"], cwd=root, capture_output=True, text=True, timeout=30)
    assert rerun.returncode != 0 and "Kept your existing BYOC setup" in rerun.stderr
    assert compose_file.read_bytes() == config and vpn.read_text() == "# team-local test settings\n"
    print("Withdrawing the reviewed image preserved the running and stopped team's setup.")
    # On a fresh machine the same no-image download starts a placeholder.
    # Refreshing its credential may recreate the agent, but keeps the service.
    compose_file.unlink()
    subprocess.run(["sh", "withdrawn.sh"], cwd=root, check=True, timeout=60)
    placeholder = docker(*compose, "ps", "-q", "service")
    assert placeholder
    subprocess.run(["sh", "renewed.sh"], cwd=root, check=True, timeout=60)
    assert docker(*compose, "ps", "-q", "service") == placeholder
    assert "-renewed" in compose_file.read_text()
    customized = compose_file.read_text().replace("    restart: unless-stopped", "    restart: always", 1)
    compose_file.write_text(customized)
    rerun = subprocess.run(["sh", "renewed.sh"], cwd=root, capture_output=True, text=True, timeout=30)
    assert rerun.returncode != 0 and "Kept your existing BYOC setup" in rerun.stderr
    assert compose_file.read_text() == customized
    assert docker(*compose, "ps", "-q", "service") == placeholder
    docker(*compose, "down", "-v", "--timeout", "1")
    compose_file.unlink()
    print("A fresh placeholder refreshed its relay credential and preserved subsequent team edits.")
    (root / "standalone.yml").rename(root / "compose.yaml")
    source_dir = root / "service-src"
    source_dir.mkdir()
    source_dir.chmod(0o755)
    source_file = source_dir / "main.py"
    source_file.write_text("original\n")
    source_file.chmod(0o644)
    stack.callback(docker, "compose", "down", "-v", "--timeout", "1", cwd=root)
    docker("compose", "up", "-d", cwd=root)
    agent = docker("compose", "ps", "-q", "rsctf-agent", cwd=root)
    wait_for_report(agent, "source baseline recorded")
    source_file.write_text("patched\n")
    wait_for_report(agent, "source changed", "main.py", "modified")
    # A setup-script download for this project must leave a separately started
    # Compose stack in its own directory, including the running agent, intact.
    docker("image", "save", "-o", root / "service.tar", f"{tag}:service")
    before = docker("compose", "ps", "-q", "service", cwd=root)
    for installer in ("setup.sh", "withdrawn.sh"):
        rerun = subprocess.run(["sh", installer], cwd=root, capture_output=True, text=True, timeout=30)
        assert rerun.returncode != 0 and "original challenge folder" in rerun.stderr
        assert docker("compose", "ps", "-q", "service", cwd=root) == before
        assert docker("compose", "ps", "-q", "rsctf-agent", cwd=root) == agent
    print("Standalone compose.yaml started the agent and observed a host source edit.")

    # A team's source mount can have unrelated mounts beneath it, including
    # individual files on the same filesystem as the registered source.
    attached = root / "attached"
    attached.mkdir()
    attached.chmod(0o755)
    (attached / "external.py").write_text("outside the source tree\n")
    (attached / "external.py").chmod(0o644)
    (source_dir / "attached").mkdir()
    (source_dir / "mounted.py").touch()
    override = root / "mounts.yaml"
    override.write_text("""services:
  rsctf-agent:
    volumes:
      - ./attached:/rsctf-source/attached:ro
      - ./attached/external.py:/rsctf-source/mounted.py:ro
""")
    overlay = ["compose", "-f", "compose.yaml", "-f", "mounts.yaml"]
    docker(*overlay, "up", "-d", "--no-deps", "--force-recreate", "rsctf-agent", cwd=root)
    agent = docker(*overlay, "ps", "-q", "rsctf-agent", cwd=root)
    wait_for_report(agent, "source baseline recorded", "files=1")
    (source_dir / ".rsctf-files").write_text("main.py\nmounted.py\n")
    (source_dir / ".rsctf-files").chmod(0o644)
    docker(*overlay, "up", "-d", "--no-deps", "--force-recreate", "rsctf-agent", cwd=root)
    agent = docker(*overlay, "ps", "-q", "rsctf-agent", cwd=root)
    wait_for_report(agent, "registered source could not be observed")
    print("Automatic discovery kept the local source and skipped separate mounts. Explicit registration failed closed.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", required=True, type=Path)
    parser.add_argument("--agent-base", required=True, help="Linux image compatible with the agent binary")
    parser.add_argument("test_command", nargs="+", help="command that runs the Rust bundle fixture")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="rsctf-byoc-source-") as directory, ExitStack() as cleanup:
        check(args, Path(directory), cleanup)
