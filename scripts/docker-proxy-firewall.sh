#!/bin/sh
# Restrict Docker's private PlatformProxy bind to the dedicated rsctf bridge.
#
# Docker may handle a host-published port through either its INPUT path
# (userland proxy) or FORWARD/DOCKER-USER (kernel DNAT). Both hooks are needed:
# protecting only DOCKER-USER leaves the userland-proxy path reachable from
# unrelated local containers.

set -eu

PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

mode=${1:-run}
bind=${RSCTF_DOCKER_PROXY_BIND:-}
subnet=${RSCTF_CHALLENGE_PROXY_SUBNET:-}
bridge=${RSCTF_CHALLENGE_PROXY_BRIDGE:-}
interval=${RSCTF_PROXY_FIREWALL_RECONCILE_SECONDS:-2}

die() {
  printf 'rsctf PlatformProxy firewall: %s\n' "$*" >&2
  exit 1
}

case "$mode" in
  run | install | check | remove) ;;
  *) die "usage: $0 [run|install|check|remove]" ;;
esac

case "$bridge" in
  '' | *[!A-Za-z0-9_.-]*)
    die "RSCTF_CHALLENGE_PROXY_BRIDGE must contain only A-Z, a-z, 0-9, dot, underscore, or dash"
    ;;
esac
[ "${#bridge}" -le 15 ] \
  || die "RSCTF_CHALLENGE_PROXY_BRIDGE exceeds Linux's 15-character interface limit"

case "$interval" in
  '' | *[!0-9]*) die "RSCTF_PROXY_FIREWALL_RECONCILE_SECONDS must be a positive integer" ;;
esac
[ "$interval" -ge 1 ] \
  || die "RSCTF_PROXY_FIREWALL_RECONCILE_SECONDS must be a positive integer"

python3 - "$bind" "$subnet" <<'PY' \
  || die "RSCTF_DOCKER_PROXY_BIND must be a usable private IPv4 address inside RSCTF_CHALLENGE_PROXY_SUBNET"
import ipaddress
import sys

try:
    address = ipaddress.ip_address(sys.argv[1])
    network = ipaddress.ip_network(sys.argv[2], strict=True)
except ValueError:
    raise SystemExit(1)

private_networks = tuple(
    ipaddress.ip_network(value)
    for value in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16")
)
if (
    address.version != 4
    or network.version != 4
    or not any(address in private for private in private_networks)
    or not any(network.subnet_of(private) for private in private_networks)
    or address.is_loopback
    or address.is_unspecified
    or address not in network
    or address in (network.network_address, network.broadcast_address)
):
    raise SystemExit(1)
PY

chain="RSCTFPP_$(printf '%s' "$bind" | tr '.' '_')"

iptables_command=''
try_iptables() {
  candidate=$1
  [ -n "$candidate" ] || return 1
  command -v "$candidate" >/dev/null 2>&1 || return 1
  if [ "$mode" = remove ]; then
    "$candidate" -w -t filter -S "$chain" >/dev/null 2>&1 \
      || "$candidate" -w -t filter -S DOCKER-USER >/dev/null 2>&1 \
      || return 1
  else
    "$candidate" -w -t filter -S DOCKER-USER >/dev/null 2>&1 \
      && "$candidate" -w -t nat -S DOCKER >/dev/null 2>&1 \
      || return 1
  fi
  iptables_command=$candidate
  return 0
}

if [ -n "${RSCTF_IPTABLES:-}" ]; then
  try_iptables "$RSCTF_IPTABLES" \
    || die "RSCTF_IPTABLES cannot access Docker's filter table"
else
  for candidate in iptables iptables-nft iptables-legacy; do
    if try_iptables "$candidate"; then
      break
    fi
  done
fi
[ -n "$iptables_command" ] \
  || die "no iptables backend can access Docker's DOCKER-USER chain"

ipt() {
  "$iptables_command" -w -t filter "$@"
}

rule_exists() {
  ipt -C "$@" >/dev/null 2>&1
}

first_jump_target() {
  ipt -L "$1" --line-numbers -n \
    | awk 'NR == 3 { print $2 }'
}

chain_exists() {
  ipt -S "$chain" >/dev/null 2>&1
}

chain_is_exact() {
  rule_exists "$chain" -i "$bridge" -s "$subnet" -j RETURN \
    && rule_exists "$chain" -j DROP \
    && [ "$(ipt -S "$chain" | awk '/^-A / { count++ } END { print count + 0 }')" -eq 2 ]
}

remove_all_rule_copies() {
  parent=$1
  shift
  while rule_exists "$parent" "$@"; do
    ipt -D "$parent" "$@"
  done
}

install_rules() {
  [ -e "/sys/class/net/$bridge" ] \
    || die "required Docker bridge interface $bridge does not exist"

  if ! chain_exists; then
    ipt -N "$chain"
  fi
  if ! chain_is_exact; then
    ipt -F "$chain"
    ipt -A "$chain" -i "$bridge" -s "$subnet" -j RETURN
    ipt -A "$chain" -j DROP
  fi

  if ! rule_exists INPUT -d "$bind" -j "$chain" \
    || [ "$(first_jump_target INPUT)" != "$chain" ]; then
    remove_all_rule_copies INPUT -d "$bind" -j "$chain"
    ipt -I INPUT 1 -d "$bind" -j "$chain"
  fi
  if ! rule_exists DOCKER-USER -m conntrack --ctdir ORIGINAL --ctorigdst "$bind" -j "$chain" \
    || [ "$(first_jump_target DOCKER-USER)" != "$chain" ]; then
    remove_all_rule_copies DOCKER-USER \
      -m conntrack --ctdir ORIGINAL --ctorigdst "$bind" -j "$chain"
    ipt -I DOCKER-USER 1 \
      -m conntrack --ctdir ORIGINAL --ctorigdst "$bind" -j "$chain"
  fi
}

check_rules() {
  [ -e "/sys/class/net/$bridge" ] \
    && chain_exists \
    && chain_is_exact \
    && rule_exists INPUT -d "$bind" -j "$chain" \
    && [ "$(first_jump_target INPUT)" = "$chain" ] \
    && rule_exists DOCKER-USER \
      -m conntrack --ctdir ORIGINAL --ctorigdst "$bind" -j "$chain" \
    && [ "$(first_jump_target DOCKER-USER)" = "$chain" ]
}

remove_rules() {
  # A stopped Docker daemon may already have removed DOCKER-USER. Remove every
  # rule that is still addressable, then delete only this bind-derived chain.
  if ipt -S INPUT >/dev/null 2>&1; then
    remove_all_rule_copies INPUT -d "$bind" -j "$chain"
  fi
  if ipt -S DOCKER-USER >/dev/null 2>&1; then
    remove_all_rule_copies DOCKER-USER \
      -m conntrack --ctdir ORIGINAL --ctorigdst "$bind" -j "$chain"
  fi
  if chain_exists; then
    ipt -F "$chain"
    ipt -X "$chain"
  fi
}

case "$mode" in
  install)
    install_rules
    check_rules || die "installed rules failed validation"
    ;;
  check)
    check_rules || exit 1
    ;;
  remove)
    remove_rules
    ;;
  run)
    install_rules
    check_rules || die "installed rules failed validation"
    trap 'remove_rules; exit 0' HUP INT TERM
    while :; do
      sleep "$interval" &
      wait $!
      if ! check_rules; then
        install_rules
        check_rules || die "rule reconciliation failed"
      fi
    done
    ;;
esac
