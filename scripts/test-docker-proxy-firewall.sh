#!/usr/bin/env bash
# Root/NET_ADMIN integration regression for the PlatformProxy host firewall.

set -Eeuo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

readonly PROXY_NETWORK=rsctf-proxy-firewall-test
readonly TARGET_NETWORK=rsctf-target-firewall-test
readonly ATTACKER_NETWORK=rsctf-attacker-firewall-test
readonly PROXY_BRIDGE=rsctfpp-test0
readonly PROXY_SUBNET=172.30.247.0/24
readonly PROXY_BIND=172.30.247.1
readonly TARGET_SUBNET=172.30.246.0/24
readonly ATTACKER_SUBNET=172.30.245.0/24
readonly PROXY_SERVER=rsctf-proxy-firewall-target
readonly DIRECT_SERVER=rsctf-direct-firewall-target
readonly ALLOWED_CLIENT=rsctf-proxy-firewall-allowed
readonly DENIED_CLIENT=rsctf-proxy-firewall-denied
readonly DIRECT_CLIENT=rsctf-direct-firewall-allowed
readonly FIREWALL_CHAIN=RSCTFPP_172_30_247_1
FIREWALL_PID=''

command -v docker >/dev/null || {
  echo 'docker is required' >&2
  exit 1
}
command -v iptables >/dev/null || {
  echo 'iptables is required' >&2
  exit 1
}
[[ $(id -u) -eq 0 ]] || {
  echo 'this integration test requires root/NET_ADMIN' >&2
  exit 1
}

for network in "$PROXY_NETWORK" "$TARGET_NETWORK" "$ATTACKER_NETWORK"; do
  if docker network inspect "$network" >/dev/null 2>&1; then
    echo "refusing to reuse existing network $network" >&2
    exit 1
  fi
done
for container in \
  "$PROXY_SERVER" "$DIRECT_SERVER" "$ALLOWED_CLIENT" "$DENIED_CLIENT" "$DIRECT_CLIENT"; do
  if docker container inspect "$container" >/dev/null 2>&1; then
    echo "refusing to reuse existing container $container" >&2
    exit 1
  fi
done
if iptables -w -t filter -S "$FIREWALL_CHAIN" >/dev/null 2>&1; then
  echo "refusing to reuse existing firewall chain $FIREWALL_CHAIN" >&2
  exit 1
fi
if ip link show "$PROXY_BRIDGE" >/dev/null 2>&1; then
  echo "refusing to reuse existing bridge $PROXY_BRIDGE" >&2
  exit 1
fi

firewall() {
  RSCTF_DOCKER_PROXY_BIND=$PROXY_BIND \
  RSCTF_CHALLENGE_PROXY_SUBNET=$PROXY_SUBNET \
  RSCTF_CHALLENGE_PROXY_BRIDGE=$PROXY_BRIDGE \
  RSCTF_PROXY_FIREWALL_RECONCILE_SECONDS=1 \
    ./scripts/docker-proxy-firewall.sh "$@"
}

start_firewall() {
  env \
    RSCTF_DOCKER_PROXY_BIND=$PROXY_BIND \
    RSCTF_CHALLENGE_PROXY_SUBNET=$PROXY_SUBNET \
    RSCTF_CHALLENGE_PROXY_BRIDGE=$PROXY_BRIDGE \
    RSCTF_PROXY_FIREWALL_RECONCILE_SECONDS=1 \
    ./scripts/docker-proxy-firewall.sh run &
  FIREWALL_PID=$!
}

cleanup() {
  if [[ -n "$FIREWALL_PID" ]]; then
    kill -TERM "$FIREWALL_PID" >/dev/null 2>&1 || true
    wait "$FIREWALL_PID" >/dev/null 2>&1 || true
  fi
  firewall remove >/dev/null 2>&1 || true
  for container in \
    "$ALLOWED_CLIENT" "$DENIED_CLIENT" "$DIRECT_CLIENT" "$PROXY_SERVER" "$DIRECT_SERVER"; do
    docker container rm -f "$container" >/dev/null 2>&1 || true
  done
  for network in "$ATTACKER_NETWORK" "$TARGET_NETWORK" "$PROXY_NETWORK"; do
    docker network rm "$network" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

docker network create \
  --driver bridge \
  --opt "com.docker.network.bridge.name=$PROXY_BRIDGE" \
  --subnet "$PROXY_SUBNET" \
  --gateway "$PROXY_BIND" \
  "$PROXY_NETWORK" >/dev/null
docker network create \
  --driver bridge --subnet "$TARGET_SUBNET" --gateway 172.30.246.1 \
  "$TARGET_NETWORK" >/dev/null
docker network create \
  --driver bridge --subnet "$ATTACKER_SUBNET" --gateway 172.30.245.1 \
  "$ATTACKER_NETWORK" >/dev/null

docker run -d \
  --name "$PROXY_SERVER" \
  --network "$TARGET_NETWORK" \
  -p "$PROXY_BIND::80" \
  nginx:alpine >/dev/null
docker run -d \
  --name "$DIRECT_SERVER" \
  --network "$TARGET_NETWORK" \
  -p 0.0.0.0::80 \
  nginx:alpine >/dev/null

proxy_port=$(docker inspect --format \
  '{{(index (index .NetworkSettings.Ports "80/tcp") 0).HostPort}}' "$PROXY_SERVER")
direct_port=$(docker inspect --format \
  '{{(index (index .NetworkSettings.Ports "80/tcp") 0).HostPort}}' "$DIRECT_SERVER")
attacker_gateway=$(docker network inspect --format \
  '{{(index .IPAM.Config 0).Gateway}}' "$ATTACKER_NETWORK")

start_firewall
for _ in {1..20}; do
  if firewall check; then
    break
  fi
  sleep 0.2
done
firewall check

# Simulate a Docker/firewall-manager rule reset. The long-running guard must
# restore its exact INPUT hook without operator intervention.
iptables -w -t filter -D INPUT -d "$PROXY_BIND" -j "$FIREWALL_CHAIN"
for _ in {1..20}; do
  if firewall check; then
    break
  fi
  sleep 0.2
done
firewall check

docker run --rm \
  --name "$ALLOWED_CLIENT" \
  --network "$PROXY_NETWORK" \
  curlimages/curl:8.12.1 \
  -fsS --max-time 3 "http://${PROXY_BIND}:${proxy_port}/" >/dev/null

if docker run --rm \
  --name "$DENIED_CLIENT" \
  --network "$ATTACKER_NETWORK" \
  curlimages/curl:8.12.1 \
  -fsS --max-time 3 "http://${PROXY_BIND}:${proxy_port}/" >/dev/null 2>&1; then
  echo 'separate Docker bridge bypassed the PlatformProxy firewall' >&2
  exit 1
fi

docker run --rm \
  --name "$DIRECT_CLIENT" \
  --network "$ATTACKER_NETWORK" \
  curlimages/curl:8.12.1 \
  -fsS --max-time 3 "http://${attacker_gateway}:${direct_port}/" >/dev/null

kill -TERM "$FIREWALL_PID"
wait "$FIREWALL_PID"
FIREWALL_PID=''
if iptables -w -t filter -S "$FIREWALL_CHAIN" >/dev/null 2>&1; then
  echo 'explicit firewall removal left its managed chain behind' >&2
  exit 1
fi

echo 'PlatformProxy firewall positive, negative, direct-mode, and cleanup checks passed.'
