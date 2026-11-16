#!/usr/bin/env python3
"""Kernel regression invoked by same_origin_ssh_kernel_regression.

Run only in the fresh network namespace supplied by that ignored Rust test.
Production INPUT rules arrive on stdin. A veth named wg0 models decrypted VPN
traffic; this tests TCP routing/fences, not WireGuard handshakes or SSH auth.
Requires Linux, root, NET_ADMIN/SYS_ADMIN, iproute2, iptables and ipset.
"""

import json
import socket
import subprocess
import sys
import threading


def run(*args):
    result = subprocess.run(args, capture_output=True, text=True, timeout=10)
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed: {result.stderr}")
    return result.stdout


rules = json.load(sys.stdin)
hub, peer, stranger, alternate = "10.13.42.1", "10.13.42.2", "10.13.42.3", "10.13.42.99"
port = 2223
client = subprocess.Popen(
    ["unshare", "-n", "python3", "-u", "-c",
     "import sys; print('ready'); sys.stdin.read()"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
)
listeners = []
try:
    assert client.stdout.readline().strip() == "ready"
    run("ip", "link", "set", "lo", "up")
    run("ip", "link", "add", "wg0", "type", "veth", "peer", "name", "client0")
    run("ip", "link", "set", "client0", "netns", str(client.pid))
    for address in (hub, alternate):
        run("ip", "addr", "add", address + "/24", "dev", "wg0")
    run("ip", "link", "set", "wg0", "up")
    prefix = ["nsenter", "-t", str(client.pid), "-n"]
    for address in (peer, stranger):
        run(*prefix, "ip", "addr", "add", address + "/24", "dev", "client0")
    run(*prefix, "ip", "link", "set", "client0", "up")
    run(*prefix, "ip", "link", "set", "lo", "up")

    def serve(listener):
        while True:
            try:
                connection, _ = listener.accept()
                with connection:
                    connection.sendall(b"SSH-2.0-routing-fixture\r\n")
            except OSError:
                return

    for listen in (port, 2222):
        listener = socket.socket()
        listener.bind(("0.0.0.0", listen))
        listener.listen()
        listeners.append(listener)
        threading.Thread(target=serve, args=(listener,), daemon=True).start()

    for name, kind in (("rsv_a_test", "hash:ip"), ("rsv_quarantine", "hash:ip"),
                       ("rsv_transition", "hash:ip,port,ip"),
                       ("rsv_capture_required", "hash:ip,port"),
                       ("rsv_capture_live", "hash:ip,port")):
        run("ipset", "create", name, kind)
    run("ipset", "add", "rsv_a_test", peer)
    run("iptables", "-N", "RSCTF_VPN_INPUT")
    run("iptables", "-A", "INPUT", "-i", "wg0", "-j", "RSCTF_VPN_INPUT")

    def install(include_ssh):
        run("iptables", "-F", "RSCTF_VPN_INPUT")
        for rule in rules:
            if not include_ssh and str(port) in rule:
                continue
            run("iptables", "-A", "RSCTF_VPN_INPUT", *rule)

    def connects(source=peer, target=hub, target_port=port):
        probe = """
import socket, sys
s = socket.socket(); s.settimeout(1); s.bind((sys.argv[1], 0))
try:
    s.connect((sys.argv[2], int(sys.argv[3])))
    assert s.recv(128).startswith(b'SSH-2.0-')
except (OSError, AssertionError):
    sys.exit(1)
"""
        return subprocess.run(
            [*prefix, "python3", "-c", probe, source, target, str(target_port)],
            check=False,
        ).returncode == 0

    install(False)
    assert not connects(), "old DNS-only INPUT rules must reproduce the SSH timeout"
    install(True)
    assert connects(), "current peer must reach the configured SSH listener"
    assert not connects(source=stranger), "unknown peers must remain blocked"
    assert not connects(target=alternate), "SSH must be limited to the hub address"
    assert not connects(target_port=2222), "other listening ports must remain blocked"
    run("ipset", "add", "rsv_quarantine", peer)
    assert not connects(), "quarantine must precede SSH admission"
    run("ipset", "del", "rsv_quarantine", peer)
    assert connects(), "removing quarantine must restore permitted SSH"
    run("ipset", "del", "rsv_a_test", peer)
    assert not connects(), "revoked peers must lose access"
    print("SSH kernel regression: old timeout reproduced; hub/custom port works; unknown, quarantined and revoked peers blocked")
finally:
    for listener in listeners:
        listener.close()
    client.stdin.close()
    client.wait(timeout=5)
