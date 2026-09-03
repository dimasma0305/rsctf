#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Compose requires these values at interpolation time. They are fixed test
# fixtures and never reach a running container because this script only renders.
export POSTGRES_PASSWORD=compose-security-test
export RSCTF_JWT_SECRET=0123456789abcdef0123456789abcdef
export RSCTF_IDENTITY_HASH_KEY=fedcba9876543210fedcba9876543210
export RSCTF_BOOTSTRAP_TOKEN=0123456789abcdef0123456789abcdef
export RSCTF_PUBLIC_URL=https://ctf.example
export RSCTF_DOCKER_PUBLIC_ENTRY=ctf.example
export RSCTF_DOCKER_SCOPE=compose-security-installation
export RSCTF_AD_VPN_SERVER_ENDPOINT=ctf.example:51820
export RSCTF_AD_VPN_SERVICES_NETWORK=rsctf-compose-security-ad
export RSCTF_DOMAIN=ctf.example
export RSCTF_TRUSTED_PROXY_CIDRS=172.31.252.0/24
export RSCTF_IMAGE=example.invalid/rsctf:test
export RSCTF_ALLOW_REGISTER=false
export RSCTF_ALLOW_PASSWORD_REGISTRATION=false
export RSCTF_EMAIL_CONFIRM=true
export RSCTF_ADMIN_CONFIRM=true
export RSCTF_ACTIVE_ON_REGISTER=false
export RSCTF_USE_CAPTCHA=true
export RSCTF_GOOGLE_CLIENT_ID=compose-google-id
export RSCTF_GOOGLE_CLIENT_SECRET=compose-google-secret
export RSCTF_DISCORD_CLIENT_ID=compose-discord-id
export RSCTF_DISCORD_CLIENT_SECRET=compose-discord-secret
export RSCTF_GOOGLE_AUTH_URL=https://google.example/authorize
export RSCTF_GOOGLE_TOKEN_URL=https://google.example/token
export RSCTF_GOOGLE_USERINFO_URL=https://google.example/userinfo
export RSCTF_DISCORD_AUTH_URL=https://discord.example/authorize
export RSCTF_DISCORD_TOKEN_URL=https://discord.example/token
export RSCTF_DISCORD_USERINFO_URL=https://discord.example/userinfo
unset RSCTF_AD_SUBMIT_BURST_FLAGS
unset RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE

compose=(docker compose --env-file /dev/null -p rsctf-compose-security)

assert_service_security() {
  local service="$1"
  local net_admin="$2"
  local net_raw="$3"
  local tun="$4"
  python3 -c '
import json
import sys

document = json.load(sys.stdin)
name, expected_admin, expected_raw, expected_tun = sys.argv[1:]
service = document["services"][name]
capabilities = set(service.get("cap_add") or [])
devices = service.get("devices") or []
has_tun = any(device.get("target") == "/dev/net/tun" for device in devices)

actual = {
    "NET_ADMIN": "NET_ADMIN" in capabilities,
    "NET_RAW": "NET_RAW" in capabilities,
    "TUN": has_tun,
}
expected = {
    "NET_ADMIN": expected_admin == "yes",
    "NET_RAW": expected_raw == "yes",
    "TUN": expected_tun == "yes",
}
if actual != expected:
    raise SystemExit(f"{name} security mismatch: expected {expected}, got {actual}")
if service.get("environment", {}).get("RSCTF_DOCKER_SCOPE") != "compose-security-installation":
    raise SystemExit(f"{name} does not inherit the installation Docker scope")
' "$service" "$net_admin" "$net_raw" "$tun"
}

assert_service_ad_submit_burst() {
  local service="$1"
  local expected="$2"
  python3 -c '
import json
import sys

document = json.load(sys.stdin)
service, expected = sys.argv[1:]
actual = document["services"][service].get("environment", {}).get(
    "RSCTF_AD_SUBMIT_BURST_FLAGS"
)
if actual != expected:
    raise SystemExit(
        f"{service} A&D submit burst mismatch: expected {expected}, got {actual}"
    )
' "$service" "$expected"
}

assert_service_koth_capability_admission() {
  local service="$1"
  local expected="$2"
  python3 -c '
import json
import sys

document = json.load(sys.stdin)
service, expected = sys.argv[1:]
actual = document["services"][service].get("environment", {}).get(
    "RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE"
)
if actual != expected:
    raise SystemExit(
        f"{service} KotH capability admission mismatch: expected {expected}, got {actual}"
    )
' "$service" "$expected"
}

assert_service_registration_oauth() {
  local service="$1"
  python3 -c '
import json
import sys

document = json.load(sys.stdin)
service = document["services"][sys.argv[1]]
environment = service.get("environment") or {}
expected = {
    "RSCTF_ALLOW_REGISTER": "false",
    "RSCTF_ALLOW_PASSWORD_REGISTRATION": "false",
    "RSCTF_EMAIL_CONFIRM": "true",
    "RSCTF_ADMIN_CONFIRM": "true",
    "RSCTF_ACTIVE_ON_REGISTER": "false",
    "RSCTF_USE_CAPTCHA": "true",
    "RSCTF_GOOGLE_CLIENT_ID": "compose-google-id",
    "RSCTF_GOOGLE_CLIENT_SECRET": "compose-google-secret",
    "RSCTF_DISCORD_CLIENT_ID": "compose-discord-id",
    "RSCTF_DISCORD_CLIENT_SECRET": "compose-discord-secret",
    "RSCTF_GOOGLE_AUTH_URL": "https://google.example/authorize",
    "RSCTF_GOOGLE_TOKEN_URL": "https://google.example/token",
    "RSCTF_GOOGLE_USERINFO_URL": "https://google.example/userinfo",
    "RSCTF_DISCORD_AUTH_URL": "https://discord.example/authorize",
    "RSCTF_DISCORD_TOKEN_URL": "https://discord.example/token",
    "RSCTF_DISCORD_USERINFO_URL": "https://discord.example/userinfo",
}

actual = {key: environment.get(key) for key in expected}
if actual != expected:
    raise SystemExit(
        f"{sys.argv[1]} registration/OAuth environment mismatch: "
        f"expected {expected}, got {actual}"
    )
' "$service"
}

assert_split_koth_reporting() {
  python3 -c '
import json
import sys

document = json.load(sys.stdin)
expected = "http://rsctf-koth-reporter:8080"
for name in ("rsctf", "rsctf-control"):
    actual = document["services"][name].get("environment", {}).get(
        "RSCTF_KOTH_REPORTER_BASE_URL"
    )
    if actual != expected:
        raise SystemExit(
            f"{name} managed KotH reporting mismatch: expected {expected}, got {actual}"
        )
aliases = (
    document["services"]["rsctf-control"]
    .get("networks", {})
    .get("rsctf-ad", {})
    .get("aliases", [])
)
if "rsctf-koth-reporter" not in aliases:
    raise SystemExit("control does not own the private managed KotH reporter alias")
'
}

assert_bounded_logs() {
  local service="$1"
  python3 -c '
import json
import sys

document = json.load(sys.stdin)
name = sys.argv[1]
logging = document["services"][name].get("logging") or {}
expected = {"driver": "json-file", "options": {"max-file": "5", "max-size": "20m"}}
if logging != expected:
    raise SystemExit(f"{name} log bounds mismatch: expected {expected}, got {logging}")
' "$service"
}

assert_same_origin_event_ingress() {
  python3 -c '
import json
import sys

document = json.load(sys.stdin)
services = document["services"]
app = services["rsctf"]
caddy = services["caddy"]
dns = services["event-vpn-dns"]

app_environment = app.get("environment") or {}
expected_app = {
    "RSCTF_TRUSTED_PROXY_CIDRS": "172.31.252.0/24,10.13.40.253/32",
    "RSCTF_EVENT_VPN_HUB_ADDRESS": "10.13.0.1",
    "RSCTF_EVENT_VPN_INGRESS_IP": "10.13.40.253",
}
for key, expected in expected_app.items():
    if app_environment.get(key) != expected:
        raise SystemExit(f"rsctf {key} mismatch: {app_environment.get(key)}")
if app.get("networks", {}).get("rsctf-ad", {}).get("ipv4_address") != "10.13.40.2":
    raise SystemExit("rsctf does not own the expected private ingress next hop")

if set(caddy.get("cap_drop") or []) != {"ALL"}:
    raise SystemExit("private Caddy ingress must drop all inherited capabilities")
if set(caddy.get("cap_add") or []) != {"NET_ADMIN", "NET_BIND_SERVICE"}:
    raise SystemExit("private Caddy ingress has unexpected capabilities")
if "no-new-privileges:true" not in (caddy.get("security_opt") or []):
    raise SystemExit("private Caddy ingress lacks no-new-privileges")
if caddy.get("networks", {}).get("rsctf-ad", {}).get("ipv4_address") != "10.13.40.253":
    raise SystemExit("private Caddy ingress address mismatch")
entrypoint = "\n".join(caddy.get("entrypoint") or [])
if "ip route replace" not in entrypoint or "RSCTF_AD_VPN_CLIENT_CIDR" not in entrypoint:
    raise SystemExit("private Caddy ingress does not install the exact VPN return route")

expected_image = (
    "coredns/coredns:1.14.7@sha256:"
    "7efd3c635b03efd68c4e8398fc45f0d993d0e9ab016f72c1cefb0fd6d01aa286"
)
actual_image = dns.get("image")
if actual_image != expected_image:
    raise SystemExit(f"event VPN DNS image is not pinned: {actual_image}")
if dns.get("network_mode") != "service:rsctf" or dns.get("ports"):
    raise SystemExit("event VPN DNS must share only the backend network namespace")
if set(dns.get("cap_drop") or []) != {"ALL"}:
    raise SystemExit("event VPN DNS must drop all inherited capabilities")
if set(dns.get("cap_add") or []) != {"NET_BIND_SERVICE"}:
    raise SystemExit("event VPN DNS has unexpected capabilities")
if dns.get("read_only") is not True or dns.get("pids_limit") != 64:
    raise SystemExit("event VPN DNS filesystem/process bounds are missing")
if "no-new-privileges:true" not in (dns.get("security_opt") or []):
    raise SystemExit("event VPN DNS lacks no-new-privileges")
if document.get("networks", {}).get("rsctf-ad", {}).get("internal") is not True:
    raise SystemExit("event VPN service network must remain internal")
'
}

assert_private_challenge_proxy() {
  local service="$1"
  python3 -c '
import ipaddress
import json
import sys

document = json.load(sys.stdin)
service = document["services"][sys.argv[1]]
bind = service.get("environment", {}).get("RSCTF_DOCKER_PROXY_BIND")
if bind != "172.31.253.1":
    raise SystemExit(f"unexpected PlatformProxy bind for {sys.argv[1]}: {bind}")
address = ipaddress.ip_address(bind)
if not address.is_private or address.is_unspecified:
    raise SystemExit(f"PlatformProxy bind is not private: {bind}")
if "challenge-proxy" not in service.get("networks", {}):
    raise SystemExit(f"{sys.argv[1]} is not attached to challenge-proxy")
network = document["networks"]["challenge-proxy"]
if network.get("internal") is True:
    raise SystemExit("challenge-proxy cannot reach its host gateway when marked internal")
bridge = network.get("driver_opts", {}).get("com.docker.network.bridge.name")
if bridge != "rsctf-proxy0":
    raise SystemExit(f"unexpected challenge-proxy bridge interface: {bridge}")
ipam = network.get("ipam", {}).get("config", [])
if len(ipam) != 1 or ipam[0].get("gateway") != bind:
    raise SystemExit(f"challenge-proxy gateway does not match bind: {ipam}")
' "$service"
}

assert_proxy_firewall() {
  local app_service="$1"
  python3 -c '
import json
import sys

document = json.load(sys.stdin)
app_name = sys.argv[1]
service = document["services"]["rsctf-proxy-firewall"]
app = document["services"][app_name]

if service.get("network_mode") != "host":
    raise SystemExit("PlatformProxy firewall does not share the host network namespace")
cap_add = set(service.get("cap_add") or [])
cap_drop = set(service.get("cap_drop") or [])
if cap_add != {"NET_ADMIN"}:
    raise SystemExit(f"PlatformProxy firewall capability mismatch: {sorted(cap_add)}")
if cap_drop != {"ALL"}:
    raise SystemExit(f"PlatformProxy firewall cap_drop mismatch: {sorted(cap_drop)}")
if service.get("privileged") is True or service.get("devices") or service.get("volumes"):
    raise SystemExit("PlatformProxy firewall has broader host authority than required")
if service.get("read_only") is not True or service.get("pids_limit") != 32:
    raise SystemExit("PlatformProxy firewall filesystem/process bounds are missing")
if "no-new-privileges:true" not in (service.get("security_opt") or []):
    raise SystemExit("PlatformProxy firewall lacks no-new-privileges")
entrypoint = service.get("entrypoint")
command = service.get("command")
if entrypoint != ["/usr/local/sbin/rsctf-proxy-firewall"]:
    raise SystemExit(f"unexpected PlatformProxy firewall entrypoint: {entrypoint}")
if command != ["run"]:
    raise SystemExit(f"unexpected PlatformProxy firewall command: {command}")
environment = service.get("environment") or {}
expected = {
    "RSCTF_DOCKER_PROXY_BIND": "172.31.253.1",
    "RSCTF_CHALLENGE_PROXY_SUBNET": "172.31.253.0/24",
    "RSCTF_CHALLENGE_PROXY_BRIDGE": "rsctf-proxy0",
}
for key, value in expected.items():
    if environment.get(key) != value:
        raise SystemExit(f"PlatformProxy firewall {key} mismatch: {environment.get(key)}")
condition = (app.get("depends_on") or {}).get("rsctf-proxy-firewall", {}).get("condition")
if condition != "service_healthy":
    raise SystemExit(f"{app_name} is not health-gated on PlatformProxy firewall")
' "$app_service"
}

"${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_service_security rsctf yes no no
"${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_private_challenge_proxy rsctf
"${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_bounded_logs db
"${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_bounded_logs redis
"${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_bounded_logs rsctf
"${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_service_ad_submit_burst rsctf 400
"${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_service_koth_capability_admission rsctf 6000
"${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_service_registration_oauth rsctf
RSCTF_AD_SUBMIT_BURST_FLAGS=3200 \
  "${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_service_ad_submit_burst rsctf 3200
RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE=3000 \
  "${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_service_koth_capability_admission rsctf 3000

"${compose[@]}" -f deploy/compose.yml -f deploy/compose.ad-vpn.yml \
  config --format json | assert_service_security rsctf yes yes yes

docker_backend=(-f deploy/compose.yml -f deploy/compose.docker.yml)
"${compose[@]}" "${docker_backend[@]}" config --format json \
  | assert_proxy_firewall rsctf
"${compose[@]}" "${docker_backend[@]}" config --format json \
  | assert_private_challenge_proxy rsctf
"${compose[@]}" "${docker_backend[@]}" config --format json \
  | assert_bounded_logs rsctf-proxy-firewall

split=(
  -f deploy/compose.yml
  -f deploy/compose.roles.yml
  -f deploy/compose.docker.yml
  -f deploy/compose.roles.ad-vpn.yml
)
"${compose[@]}" "${split[@]}" config --format json \
  | assert_service_security rsctf no no no
"${compose[@]}" "${split[@]}" config --format json \
  | assert_service_registration_oauth rsctf
"${compose[@]}" "${split[@]}" config --format json \
  | assert_service_ad_submit_burst rsctf 400
"${compose[@]}" "${split[@]}" config --format json \
  | assert_service_koth_capability_admission rsctf 6000
RSCTF_AD_SUBMIT_BURST_FLAGS=3200 \
  "${compose[@]}" "${split[@]}" config --format json \
  | assert_service_ad_submit_burst rsctf 3200
RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE=3000 \
  "${compose[@]}" "${split[@]}" config --format json \
  | assert_service_koth_capability_admission rsctf 3000
"${compose[@]}" "${split[@]}" config --format json \
  | assert_service_security rsctf-control yes yes yes
"${compose[@]}" "${split[@]}" config --format json \
  | assert_service_registration_oauth rsctf-control
"${compose[@]}" "${split[@]}" config --format json \
  | assert_private_challenge_proxy rsctf-control
"${compose[@]}" "${split[@]}" config --format json \
  | assert_bounded_logs rsctf-control
"${compose[@]}" "${split[@]}" config --format json \
  | assert_split_koth_reporting

split_docker=(
  -f deploy/compose.yml
  -f deploy/compose.roles.yml
  -f deploy/compose.docker.yml
  -f deploy/compose.roles.docker.yml
)
"${compose[@]}" "${split_docker[@]}" config --format json \
  | assert_proxy_firewall rsctf-control
"${compose[@]}" -f deploy/compose.yml -f deploy/compose.caddy.yml config --format json \
  | assert_bounded_logs caddy

same_origin=(
  -f deploy/compose.yml
  -f deploy/compose.caddy.yml
  -f deploy/compose.ad-vpn.yml
  -f deploy/compose.event-vpn-ingress.yml
)
"${compose[@]}" "${same_origin[@]}" config --format json \
  | assert_same_origin_event_ingress

echo "Compose capability ownership and log bounds are valid."
