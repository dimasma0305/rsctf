#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Compose requires these values at interpolation time. They are fixed test
# fixtures and never reach a running container because this script only renders.
export POSTGRES_PASSWORD=compose-security-test
export RSCTF_JWT_SECRET=0123456789abcdef0123456789abcdef
export RSCTF_BOOTSTRAP_TOKEN=0123456789abcdef0123456789abcdef
export RSCTF_PUBLIC_URL=https://ctf.example
export RSCTF_DOCKER_PUBLIC_ENTRY=ctf.example
export RSCTF_DOCKER_SCOPE=compose-security-installation
export RSCTF_AD_VPN_SERVER_ENDPOINT=ctf.example:51820
export RSCTF_AD_VPN_SERVICES_NETWORK=rsctf-compose-security-ad
export RSCTF_IMAGE=example.invalid/rsctf:test
unset RSCTF_AD_SUBMIT_BURST_FLAGS

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

"${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_service_security rsctf yes no no
"${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_service_ad_submit_burst rsctf 400
RSCTF_AD_SUBMIT_BURST_FLAGS=3200 \
  "${compose[@]}" -f deploy/compose.yml config --format json \
  | assert_service_ad_submit_burst rsctf 3200

"${compose[@]}" -f deploy/compose.yml -f deploy/compose.ad-vpn.yml \
  config --format json | assert_service_security rsctf yes yes yes

split=(
  -f deploy/compose.yml
  -f deploy/compose.roles.yml
  -f deploy/compose.docker.yml
  -f deploy/compose.roles.ad-vpn.yml
)
"${compose[@]}" "${split[@]}" config --format json \
  | assert_service_security rsctf no no no
"${compose[@]}" "${split[@]}" config --format json \
  | assert_service_ad_submit_burst rsctf 400
RSCTF_AD_SUBMIT_BURST_FLAGS=3200 \
  "${compose[@]}" "${split[@]}" config --format json \
  | assert_service_ad_submit_burst rsctf 3200
"${compose[@]}" "${split[@]}" config --format json \
  | assert_service_security rsctf-control yes yes yes

echo "Compose capability ownership is valid."
