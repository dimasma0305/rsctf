#!/usr/bin/env bash
# Guided Docker installer for rsctf.

set -Eeuo pipefail
IFS=$'\n\t'

readonly DEFAULT_BUNDLE_ROOT="https://raw.githubusercontent.com/dimasma0305/rsctf"
readonly DEFAULT_IMAGE="dimasmaualana/rsctf:latest"

ORIGINAL_ARGS=("$@")
MODE=""
DOMAIN=""
PUBLIC_URL=""
HTTP_PORT="8080"
HTTP_BIND_IP="127.0.0.1"
TRUSTED_PROXY_CIDRS=""
PUBLIC_ENTRY=""
RSCTF_IMAGE="${RSCTF_IMAGE:-$DEFAULT_IMAGE}"
INSTALL_DIR="${RSCTF_INSTALL_DIR:-}"
GIT_REF="${RSCTF_REF:-main}"
BUNDLE_URL="${RSCTF_BUNDLE_URL:-}"
HEALTH_TIMEOUT="180"
DOCKER_BACKEND=-1
AD_VPN=0
NON_INTERACTIVE=0
CONFIGURE_ONLY=0
DOCTOR_ONLY=0

if [[ -t 1 ]]; then
  BOLD=$'\033[1m'
  GREEN=$'\033[32m'
  YELLOW=$'\033[33m'
  RED=$'\033[31m'
  RESET=$'\033[0m'
else
  BOLD=""
  GREEN=""
  YELLOW=""
  RED=""
  RESET=""
fi

info() { printf '%s==>%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '%sWarning:%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die() { printf '%sError:%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Install rsctf with Docker Compose.

Usage:
  scripts/install.sh [options]

Modes:
  --mode local          Plain HTTP bound to localhost (default)
  --mode caddy          Public Caddy proxy with automatic HTTPS
  --mode proxy          Run behind an existing reverse proxy

Options:
  --domain HOST         Public hostname (required for Caddy mode)
  --public-url URL      Canonical browser URL (required for proxy mode unless
                        --domain is supplied)
  --port PORT           Host HTTP port for rsctf (default: 8080)
  --bind ADDRESS        Host bind IPv4 address (default: 127.0.0.1)
  --trusted-proxy-cidrs CIDRS
                        Comma-separated proxy CIDRs allowed to set client IPs
  --with-docker         Enable Docker-backed challenge containers
  --without-docker      Explicitly keep the challenge backend disabled
  --with-ad-vpn         Enable A&D WireGuard + SSH (implies --with-docker)
  --public-entry HOST   Public DNS name or IPv4 address for challenge/VPN ports
  --image IMAGE         rsctf image (default: dimasmaualana/rsctf:latest)
  --timeout SECONDS     Health-check wait limit (default: 180)
  --configure-only      Write/validate deploy/.env without starting containers
  --doctor              Check this host and configuration, then exit
  --non-interactive     Use flags and safe defaults; never prompt
  --yes                 Alias for --non-interactive

Bootstrap options (also make a curl-piped installer possible):
  --install-dir PATH    Deployment-bundle destination (default: ./rsctf)
  --bundle-url URL      Raw GitHub base URL containing deploy/ and scripts/
  --ref REF             GitHub branch or tag (default: main)
  -h, --help            Show this help

Examples:
  ./scripts/install.sh
  ./scripts/install.sh --mode caddy --domain ctf.example.com --with-docker
  ./scripts/install.sh --mode proxy --public-url https://ctf.example.com \
    --trusted-proxy-cidrs 127.0.0.1/32 --with-docker --non-interactive
  ./scripts/install.sh --doctor
EOF
}

need_value() {
  [[ $# -ge 2 && -n "$2" ]] || die "$1 requires a value"
}

while (($#)); do
  case "$1" in
    --mode) need_value "$1" "${2:-}"; MODE=$2; shift 2 ;;
    --mode=*) MODE=${1#*=}; shift ;;
    --domain) need_value "$1" "${2:-}"; DOMAIN=$2; shift 2 ;;
    --domain=*) DOMAIN=${1#*=}; shift ;;
    --public-url) need_value "$1" "${2:-}"; PUBLIC_URL=$2; shift 2 ;;
    --public-url=*) PUBLIC_URL=${1#*=}; shift ;;
    --port) need_value "$1" "${2:-}"; HTTP_PORT=$2; shift 2 ;;
    --port=*) HTTP_PORT=${1#*=}; shift ;;
    --bind) need_value "$1" "${2:-}"; HTTP_BIND_IP=$2; shift 2 ;;
    --bind=*) HTTP_BIND_IP=${1#*=}; shift ;;
    --trusted-proxy-cidrs) need_value "$1" "${2:-}"; TRUSTED_PROXY_CIDRS=$2; shift 2 ;;
    --trusted-proxy-cidrs=*) TRUSTED_PROXY_CIDRS=${1#*=}; shift ;;
    --public-entry) need_value "$1" "${2:-}"; PUBLIC_ENTRY=$2; shift 2 ;;
    --public-entry=*) PUBLIC_ENTRY=${1#*=}; shift ;;
    --image) need_value "$1" "${2:-}"; RSCTF_IMAGE=$2; shift 2 ;;
    --image=*) RSCTF_IMAGE=${1#*=}; shift ;;
    --timeout) need_value "$1" "${2:-}"; HEALTH_TIMEOUT=$2; shift 2 ;;
    --timeout=*) HEALTH_TIMEOUT=${1#*=}; shift ;;
    --install-dir) need_value "$1" "${2:-}"; INSTALL_DIR=$2; shift 2 ;;
    --install-dir=*) INSTALL_DIR=${1#*=}; shift ;;
    --bundle-url) need_value "$1" "${2:-}"; BUNDLE_URL=${2%/}; shift 2 ;;
    --bundle-url=*) BUNDLE_URL=${1#*=}; BUNDLE_URL=${BUNDLE_URL%/}; shift ;;
    --ref) need_value "$1" "${2:-}"; GIT_REF=$2; shift 2 ;;
    --ref=*) GIT_REF=${1#*=}; shift ;;
    --with-docker) DOCKER_BACKEND=1; shift ;;
    --without-docker) DOCKER_BACKEND=0; shift ;;
    --with-ad-vpn) AD_VPN=1; DOCKER_BACKEND=1; shift ;;
    --configure-only|--no-start) CONFIGURE_ONLY=1; shift ;;
    --doctor) DOCTOR_ONLY=1; NON_INTERACTIVE=1; shift ;;
    --non-interactive|--yes) NON_INTERACTIVE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    --) shift; break ;;
    *) die "unknown option: $1 (try --help)" ;;
  esac
done

[[ $# -eq 0 ]] || die "unexpected positional argument: $1"

unset CDPATH
if ! SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" 2>/dev/null && pwd); then
  SCRIPT_DIR=""
fi
if ! REPO_ROOT=$(cd -- "$SCRIPT_DIR/.." 2>/dev/null && pwd); then
  REPO_ROOT=""
fi

download_file() {
  local url=$1 destination=$2 temporary
  temporary="${destination}.tmp.$$"
  if command -v curl >/dev/null 2>&1; then
    curl --fail --silent --show-error --location "$url" --output "$temporary"
  elif command -v wget >/dev/null 2>&1; then
    wget -q "$url" -O "$temporary"
  else
    die "curl or wget is required to download the deployment bundle"
  fi
  mv "$temporary" "$destination"
}

bootstrap_bundle() {
  local target=${INSTALL_DIR:-"$PWD/rsctf"}
  local base=${BUNDLE_URL:-"$DEFAULT_BUNDLE_ROOT/$GIT_REF"}
  local file
  local -a files=(
    deploy/compose.yml
    deploy/compose.docker.yml
    deploy/compose.caddy.yml
    deploy/compose.ad-vpn.yml
    deploy/compose.capture.yml
    deploy/compose.roles.yml
    deploy/compose.roles.docker.yml
    deploy/compose.roles.ad-vpn.yml
    deploy/compose.roles.capture.yml
    deploy/compose.workers.yml
    deploy/compose.roles.workers.yml
    deploy/Caddyfile
    deploy/postgres/init/00-pg-stat-statements.sql
    deploy/.env.example
    deploy/.gitignore
    deploy/README.md
    scripts/install.sh
  )
  info "Downloading the rsctf deployment bundle from GitHub into $target"
  mkdir -p "$target/deploy/postgres/init" "$target/scripts"
  for file in "${files[@]}"; do
    download_file "$base/$file" "$target/$file"
  done
  chmod +x "$target/scripts/install.sh"
  exec "$target/scripts/install.sh" "${ORIGINAL_ARGS[@]}"
}

if [[ ! -f "$REPO_ROOT/deploy/compose.yml" ]]; then
  bootstrap_bundle
fi

readonly REPO_ROOT
readonly DEPLOY_DIR="$REPO_ROOT/deploy"
readonly ENV_FILE="$DEPLOY_DIR/.env"

compose() {
  (cd "$DEPLOY_DIR" && docker compose "$@")
}

env_get() {
  local key=$1 file=${2:-$ENV_FILE}
  [[ -f "$file" ]] || return 0
  awk -v key="$key" 'index($0, key "=") == 1 { print substr($0, length(key) + 2); exit }' "$file"
}

env_has() {
  local key=$1
  [[ -f "$ENV_FILE" ]] && grep -q "^${key}=" "$ENV_FILE"
}

random_hex() {
  local bytes=$1
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex "$bytes"
  else
    od -An -N "$bytes" -tx1 /dev/urandom | tr -d ' \n'
  fi
}

ask_yes_no() {
  local question=$1 default=$2 answer suffix
  if [[ "$default" == "yes" ]]; then suffix="[Y/n]"; else suffix="[y/N]"; fi
  read -r -u 3 -p "$question $suffix " answer
  answer=${answer:-$default}
  [[ "$answer" =~ ^[Yy]([Ee][Ss])?$ ]]
}

prompt_configuration() {
  [[ -r /dev/tty ]] || die "interactive setup needs a terminal; use --non-interactive with flags"
  exec 3</dev/tty
  printf '\n%srsctf setup%s\n' "$BOLD" "$RESET"
  if [[ -z "$MODE" ]]; then
    printf 'Choose how browsers will reach rsctf:\n'
    printf '  1) Local HTTP (localhost only)\n'
    printf '  2) Caddy automatic HTTPS (recommended for a public server)\n'
    printf '  3) Existing reverse proxy\n'
    local choice
    read -r -u 3 -p 'Mode [1]: ' choice
    case "${choice:-1}" in
      1) MODE=local ;;
      2) MODE=caddy ;;
      3) MODE=proxy ;;
      *) die "choose 1, 2, or 3" ;;
    esac
  fi

  if [[ "$MODE" == "caddy" && -z "$DOMAIN" ]]; then
    read -r -u 3 -p 'Public domain (DNS must point to this server): ' DOMAIN
  elif [[ "$MODE" == "proxy" && -z "$PUBLIC_URL" && -z "$DOMAIN" ]]; then
    read -r -u 3 -p 'Public URL (for example https://ctf.example.com): ' PUBLIC_URL
  fi
  if [[ "$MODE" == "proxy" && -z "$TRUSTED_PROXY_CIDRS" ]]; then
    read -r -u 3 -p 'Trusted proxy CIDR(s), comma-separated (blank trusts none): ' TRUSTED_PROXY_CIDRS
  elif [[ "$MODE" == "local" ]]; then
    local entered_port
    read -r -u 3 -p "Local HTTP port [$HTTP_PORT]: " entered_port
    HTTP_PORT=${entered_port:-$HTTP_PORT}
  fi

  if [[ $DOCKER_BACKEND -lt 0 ]]; then
    if ask_yes_no 'Enable Docker-backed challenge containers? (Docker socket grants host-level control)' no; then
      DOCKER_BACKEND=1
    else
      DOCKER_BACKEND=0
    fi
  fi

  if [[ $DOCKER_BACKEND -eq 1 && $AD_VPN -eq 0 ]] \
    && ask_yes_no 'Enable the Attack-Defense WireGuard VPN and SSH bastion?' no; then
    AD_VPN=1
  fi

  if [[ $DOCKER_BACKEND -eq 1 && -z "$PUBLIC_ENTRY" ]]; then
    local default_entry=${DOMAIN:-localhost}
    local entered_entry
    read -r -u 3 -p "Public host/IP for challenge ports [$default_entry]: " entered_entry
    PUBLIC_ENTRY=${entered_entry:-$default_entry}
  fi
  exec 3<&-
}

validate_port() {
  local value=$1 label=$2
  if [[ ! "$value" =~ ^[0-9]+$ ]] || ((value < 1 || value > 65535)); then
    die "$label must be an integer from 1 to 65535"
  fi
}

valid_public_url() {
  [[ "$1" =~ ^https?://[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?(:[0-9]{1,5})?/?$ ]]
}

validate_inputs() {
  case "$MODE" in local|caddy|proxy) ;; *) die "--mode must be local, caddy, or proxy" ;; esac
  validate_port "$HTTP_PORT" "HTTP port"
  validate_port "$HEALTH_TIMEOUT" "timeout"
  [[ "$HTTP_BIND_IP" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] \
    || die "--bind must be an IPv4 address"
  [[ "$RSCTF_IMAGE" =~ ^[A-Za-z0-9._/@:-]+$ ]] || die "image contains unsupported characters"

  if [[ "$MODE" == "caddy" ]]; then
    [[ "$DOMAIN" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]] \
      || die "Caddy mode requires a valid --domain without a scheme or path"
    PUBLIC_URL="https://$DOMAIN"
  elif [[ "$MODE" == "proxy" ]]; then
    if [[ -z "$PUBLIC_URL" && -n "$DOMAIN" ]]; then PUBLIC_URL="https://$DOMAIN"; fi
    valid_public_url "$PUBLIC_URL" || die "proxy mode requires an HTTP(S) origin in --public-url (or --domain)"
  else
    PUBLIC_URL=${PUBLIC_URL:-"http://localhost:$HTTP_PORT"}
  fi

  valid_public_url "$PUBLIC_URL" || die "public URL must be an HTTP(S) origin without a path"
  [[ "$TRUSTED_PROXY_CIDRS" =~ ^[A-Fa-f0-9:.,/]*$ ]] \
    || die "trusted proxy CIDRs contain unsupported characters (do not include spaces)"

  if [[ $AD_VPN -eq 1 ]]; then DOCKER_BACKEND=1; fi
  if [[ $DOCKER_BACKEND -lt 0 ]]; then DOCKER_BACKEND=0; fi
  if [[ $DOCKER_BACKEND -eq 1 ]]; then
    if [[ -z "$PUBLIC_ENTRY" ]]; then
      PUBLIC_ENTRY=${DOMAIN:-}
      if [[ -z "$PUBLIC_ENTRY" ]]; then
        PUBLIC_ENTRY=${PUBLIC_URL#*://}
        PUBLIC_ENTRY=${PUBLIC_ENTRY%%/*}
        PUBLIC_ENTRY=${PUBLIC_ENTRY%%:*}
      fi
    fi
    PUBLIC_ENTRY=${PUBLIC_ENTRY:-localhost}
    [[ "$PUBLIC_ENTRY" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]] \
      || die "public entry must be a DNS name or IPv4 address without a scheme or port"
  fi

  if [[ $AD_VPN -eq 1 && "$PUBLIC_ENTRY" == "localhost" ]]; then
    die "A&D VPN needs a player-reachable --public-entry"
  fi
}

compose_file_value() {
  local compose_files="compose.yml"
  if [[ "$MODE" == "caddy" ]]; then compose_files+=":compose.caddy.yml"; fi
  if [[ $AD_VPN -eq 1 ]]; then
    compose_files+=":compose.ad-vpn.yml"
  elif [[ $DOCKER_BACKEND -eq 1 ]]; then
    compose_files+=":compose.docker.yml"
  fi
  printf '%s\n' "$compose_files"
}

cookie_secure_value() {
  if [[ "$PUBLIC_URL" == https://* ]]; then printf 'true\n'; else printf 'false\n'; fi
}

write_new_environment() {
  local postgres_password jwt_secret files proxy_subnet
  postgres_password=$(random_hex 24)
  jwt_secret=$(random_hex 32)
  files=$(compose_file_value)
  proxy_subnet="172.31.252.0/24"
  if [[ "$MODE" == "caddy" && -z "$TRUSTED_PROXY_CIDRS" ]]; then
    TRUSTED_PROXY_CIDRS="172.31.252.2/32"
  fi

  umask 077
  {
    printf 'COMPOSE_PROJECT_NAME=rsctf\n'
    printf 'COMPOSE_FILE=%s\n' "$files"
    printf 'RSCTF_IMAGE=%s\n' "$RSCTF_IMAGE"
    printf '\nPOSTGRES_USER=rsctf\nPOSTGRES_DB=rsctf\n'
    printf 'POSTGRES_PASSWORD=%s\n' "$postgres_password"
    printf 'RSCTF_JWT_SECRET=%s\n' "$jwt_secret"
    printf '\nRSCTF_PUBLIC_URL=%s\n' "$PUBLIC_URL"
    printf 'RSCTF_COOKIE_SECURE=%s\n' "$(cookie_secure_value)"
    printf 'RSCTF_HTTP_BIND_IP=%s\n' "$HTTP_BIND_IP"
    printf 'RSCTF_HTTP_PORT=%s\n' "$HTTP_PORT"
    printf 'RSCTF_TRUSTED_PROXY_CIDRS=%s\n' "$TRUSTED_PROXY_CIDRS"
    printf '\nRUST_LOG=info\nREDIS_MAXMEMORY=256mb\n'
    printf 'RSCTF_DB_MAX_CONNECTIONS=32\nRSCTF_PROVISIONING_CONCURRENCY=4\n'
    printf 'RSCTF_CONTAINER_MAX_MEMORY_MB=4096\nRSCTF_CONTAINER_MAX_CPU_COUNT=8\n'
    printf '\nRSCTF_DOCKER_PUBLIC_ENTRY=%s\n' "${PUBLIC_ENTRY:-localhost}"
    printf 'RSCTF_DOMAIN=%s\n' "${DOMAIN:-localhost}"
    printf 'RSCTF_PROXY_SUBNET=%s\n' "$proxy_subnet"
    printf 'RSCTF_CADDY_IP=172.31.252.2\n'
    printf '\nRSCTF_AD_VPN_SERVICES_NETWORK=rsctf-ad\n'
    printf 'RSCTF_AD_VPN_CLIENT_CIDR=10.13.0.0/19\n'
    printf 'RSCTF_AD_VPN_SERVICES_CIDR=10.13.40.0/24\n'
    printf 'RSCTF_AD_VPN_SERVER_ENDPOINT=%s:51820\n' "${PUBLIC_ENTRY:-localhost}"
    printf 'RSCTF_AD_VPN_PORT=51820\nRSCTF_AD_SSH_PORT=2222\n'
  } >"$ENV_FILE"
  chmod 600 "$ENV_FILE"
  info "Created $ENV_FILE with new random database and JWT secrets"
}

append_env_if_missing() {
  local key=$1 value=$2
  if ! env_has "$key"; then
    printf '%s=%s\n' "$key" "$value" >>"$ENV_FILE"
  fi
}

complete_existing_environment() {
  warn "Keeping the existing $ENV_FILE; existing values and secrets will not be replaced"
  umask 077
  append_env_if_missing COMPOSE_PROJECT_NAME rsctf
  append_env_if_missing COMPOSE_FILE "$(compose_file_value)"
  append_env_if_missing RSCTF_IMAGE "$RSCTF_IMAGE"
  append_env_if_missing POSTGRES_USER rsctf
  append_env_if_missing POSTGRES_DB rsctf
  append_env_if_missing POSTGRES_PASSWORD "$(random_hex 24)"
  append_env_if_missing RSCTF_JWT_SECRET "$(random_hex 32)"
  append_env_if_missing RSCTF_PUBLIC_URL "$PUBLIC_URL"
  append_env_if_missing RSCTF_COOKIE_SECURE "$(cookie_secure_value)"
  append_env_if_missing RSCTF_HTTP_BIND_IP "$HTTP_BIND_IP"
  append_env_if_missing RSCTF_HTTP_PORT "$HTTP_PORT"
  if [[ "$(env_get COMPOSE_FILE)" == *compose.caddy.yml* && -z "$TRUSTED_PROXY_CIDRS" ]]; then
    TRUSTED_PROXY_CIDRS=172.31.252.2/32
  fi
  append_env_if_missing RSCTF_TRUSTED_PROXY_CIDRS "$TRUSTED_PROXY_CIDRS"
  append_env_if_missing RUST_LOG info
  append_env_if_missing REDIS_MAXMEMORY 256mb
  append_env_if_missing RSCTF_DB_MAX_CONNECTIONS 32
  append_env_if_missing RSCTF_PROVISIONING_CONCURRENCY 4
  append_env_if_missing RSCTF_CONTAINER_MAX_MEMORY_MB 4096
  append_env_if_missing RSCTF_CONTAINER_MAX_CPU_COUNT 8
  append_env_if_missing RSCTF_DOCKER_PUBLIC_ENTRY "${PUBLIC_ENTRY:-localhost}"
  append_env_if_missing RSCTF_DOMAIN "${DOMAIN:-localhost}"
  append_env_if_missing RSCTF_PROXY_SUBNET 172.31.252.0/24
  append_env_if_missing RSCTF_CADDY_IP 172.31.252.2
  append_env_if_missing RSCTF_AD_VPN_SERVICES_NETWORK rsctf-ad
  append_env_if_missing RSCTF_AD_VPN_CLIENT_CIDR 10.13.0.0/19
  append_env_if_missing RSCTF_AD_VPN_SERVICES_CIDR 10.13.40.0/24
  append_env_if_missing RSCTF_AD_VPN_SERVER_ENDPOINT "${PUBLIC_ENTRY:-localhost}:51820"
  append_env_if_missing RSCTF_AD_VPN_PORT 51820
  append_env_if_missing RSCTF_AD_SSH_PORT 2222
  chmod 600 "$ENV_FILE"
}

guard_missing_environment_with_existing_data() {
  [[ -f "$ENV_FILE" ]] && return 0
  command -v docker >/dev/null 2>&1 || return 0
  docker info >/dev/null 2>&1 || return 0

  local volume
  while IFS= read -r volume; do
    [[ -z "$volume" ]] && continue
    die "found existing rsctf data volume '$volume' but $ENV_FILE is missing. Restore the original .env (especially POSTGRES_PASSWORD and RSCTF_JWT_SECRET) before continuing"
  done < <(
    docker volume ls --quiet \
      --filter label=com.docker.compose.project=rsctf \
      --filter label=com.docker.compose.volume=postgres_data
  )
}

check_environment_values() {
  local jwt password public_url files
  jwt=$(env_get RSCTF_JWT_SECRET)
  password=$(env_get POSTGRES_PASSWORD)
  public_url=$(env_get RSCTF_PUBLIC_URL)
  files=$(env_get COMPOSE_FILE)

  [[ ${#jwt} -ge 32 ]] || die "RSCTF_JWT_SECRET in deploy/.env must be at least 32 characters"
  [[ "$jwt" != "change-me-in-production" && "$jwt" != "insecure-dev-secret-change-me" ]] \
    || die "replace the insecure RSCTF_JWT_SECRET in deploy/.env"
  [[ "$password" =~ ^[A-Za-z0-9._~-]+$ ]] \
    || die "POSTGRES_PASSWORD must be non-empty and URL-safe (letters, numbers, . _ ~ -)"
  valid_public_url "$public_url" \
    || die "RSCTF_PUBLIC_URL in deploy/.env must be an HTTP(S) origin without a path"
  [[ -n "$files" ]] || die "COMPOSE_FILE is missing from deploy/.env"

  if [[ "$files" == *compose.caddy.yml* ]]; then
    [[ -n "$(env_get RSCTF_DOMAIN)" ]] || die "RSCTF_DOMAIN is required by the Caddy override"
    [[ -n "$(env_get RSCTF_TRUSTED_PROXY_CIDRS)" ]] \
      || die "Caddy requires RSCTF_TRUSTED_PROXY_CIDRS (normally the RSCTF_PROXY_SUBNET)"
  fi
  if [[ "$files" == *compose.docker.yml* || "$files" == *compose.ad-vpn.yml* ]]; then
    [[ -n "$(env_get RSCTF_DOCKER_PUBLIC_ENTRY)" ]] \
      || die "the Docker backend requires RSCTF_DOCKER_PUBLIC_ENTRY"
  fi
  if [[ "$files" == *compose.ad-vpn.yml* ]]; then
    [[ -n "$(env_get RSCTF_AD_VPN_SERVER_ENDPOINT)" ]] \
      || die "the A&D VPN requires RSCTF_AD_VPN_SERVER_ENDPOINT"
  fi
}

validate_compose() {
  check_environment_values
  info "Validating the Docker Compose configuration"
  compose config --quiet
}

preflight() {
  command -v docker >/dev/null 2>&1 || die "Docker is not installed; install Docker Engine with the Compose plugin first"
  docker compose version >/dev/null 2>&1 || die "Docker Compose v2 is required (the 'docker compose' command)"
  if [[ $CONFIGURE_ONLY -eq 0 ]]; then
    docker info >/dev/null 2>&1 \
      || die "cannot reach the Docker daemon; start Docker or grant this user Docker access"
  fi

  local files
  files=$(env_get COMPOSE_FILE)
  if [[ $CONFIGURE_ONLY -eq 0 && ( "$files" == *compose.docker.yml* || "$files" == *compose.ad-vpn.yml* ) ]]; then
    [[ -S /var/run/docker.sock ]] || die "the Docker backend needs /var/run/docker.sock"
  fi
  if [[ "$files" == *compose.ad-vpn.yml* ]]; then
    [[ "$(uname -s)" == Linux ]] || die "the A&D WireGuard mode requires a Linux host"
    [[ -c /dev/net/tun ]] || die "the A&D VPN needs /dev/net/tun on the host"
    if [[ ! -d /sys/module/wireguard ]]; then
      warn "the WireGuard kernel module is not visible; load it with 'sudo modprobe wireguard' if startup fails"
    fi
  fi
}

doctor() {
  local failures=0
  printf '%srsctf deployment doctor%s\n' "$BOLD" "$RESET"
  if command -v docker >/dev/null 2>&1; then
    printf '  %sok%s  Docker CLI\n' "$GREEN" "$RESET"
  else
    printf '  %sfail%s Docker CLI not found\n' "$RED" "$RESET"; failures=$((failures + 1))
  fi
  if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then
    printf '  %sok%s  Docker Compose v2\n' "$GREEN" "$RESET"
  else
    printf '  %sfail%s Docker Compose v2 unavailable\n' "$RED" "$RESET"; failures=$((failures + 1))
  fi
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    printf '  %sok%s  Docker daemon access\n' "$GREEN" "$RESET"
  else
    printf '  %sfail%s Docker daemon unavailable or permission denied\n' "$RED" "$RESET"; failures=$((failures + 1))
  fi
  if [[ -f "$ENV_FILE" ]]; then
    printf '  %sok%s  deploy/.env exists\n' "$GREEN" "$RESET"
    if (check_environment_values) >/dev/null 2>&1; then
      printf '  %sok%s  required settings and secrets\n' "$GREEN" "$RESET"
    else
      printf '  %sfail%s invalid deploy/.env values\n' "$RED" "$RESET"; failures=$((failures + 1))
    fi
    if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1 \
      && compose config --quiet >/dev/null 2>&1; then
      printf '  %sok%s  Compose configuration renders\n' "$GREEN" "$RESET"
    else
      printf '  %sfail%s Compose configuration does not render\n' "$RED" "$RESET"; failures=$((failures + 1))
    fi
    local files
    files=$(env_get COMPOSE_FILE)
    if [[ "$files" == *compose.docker.yml* || "$files" == *compose.ad-vpn.yml* ]]; then
      if [[ -S /var/run/docker.sock ]]; then
        printf '  %sok%s  Docker socket present\n' "$GREEN" "$RESET"
      else
        printf '  %sfail%s Docker socket missing\n' "$RED" "$RESET"; failures=$((failures + 1))
      fi
    fi
    if [[ "$files" == *compose.ad-vpn.yml* ]]; then
      if [[ -c /dev/net/tun ]]; then
        printf '  %sok%s  /dev/net/tun present\n' "$GREEN" "$RESET"
      else
        printf '  %sfail%s /dev/net/tun missing\n' "$RED" "$RESET"; failures=$((failures + 1))
      fi
    fi
  else
    printf '  %swarn%s deploy/.env has not been created; run the installer\n' "$YELLOW" "$RESET"
  fi
  ((failures == 0)) || return 1
}

pull_images() {
  local service
  info "Pulling dependency images"
  while IFS= read -r service; do
    [[ "$service" == rsctf ]] && continue
    compose pull "$service"
  done < <(compose config --services)

  local configured_image
  configured_image=$(env_get RSCTF_IMAGE)
  info "Pulling ${configured_image:-$DEFAULT_IMAGE}"
  compose pull rsctf || die "could not pull the rsctf image. Check --image/RSCTF_IMAGE, registry access, and network connectivity; end-user installs require a published image"
}

wait_for_health() {
  local deadline=$((SECONDS + HEALTH_TIMEOUT))
  info "Waiting up to ${HEALTH_TIMEOUT}s for rsctf /healthz"
  while ((SECONDS < deadline)); do
    if compose exec -T rsctf python3 -c \
      "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/healthz', timeout=3).read()" \
      >/dev/null 2>&1; then
      return 0
    fi
    sleep 3
  done
  warn "rsctf did not become healthy in time; recent logs follow"
  compose logs --tail 100 rsctf >&2 || true
  return 1
}

main() {
  if [[ $DOCTOR_ONLY -eq 1 ]]; then
    doctor
    exit
  fi

  if [[ -f "$ENV_FILE" ]]; then
    MODE=${MODE:-local}
    validate_inputs
    complete_existing_environment
  else
    guard_missing_environment_with_existing_data
    if [[ $NON_INTERACTIVE -eq 0 ]]; then
      prompt_configuration
    else
      MODE=${MODE:-local}
    fi
    validate_inputs
    write_new_environment
  fi

  preflight
  validate_compose
  if [[ $CONFIGURE_ONLY -eq 1 ]]; then
    printf '\n%sConfiguration is ready.%s No containers were started.\n' "$BOLD" "$RESET"
    printf 'Start later with: cd %q && docker compose up -d\n' "$DEPLOY_DIR"
    return
  fi

  pull_images
  info "Starting rsctf"
  compose up -d --remove-orphans --pull never
  wait_for_health

  local url
  url=$(env_get RSCTF_PUBLIC_URL)
  printf '\n%srsctf is ready: %s%s\n' "$BOLD" "$url" "$RESET"
  printf '%sImportant:%s register the first account now; the first account in a new database becomes the active administrator.\n' "$YELLOW" "$RESET"
  printf 'Manage it with: cd %q && docker compose ps\n' "$DEPLOY_DIR"
}

main
