#!/usr/bin/env bash

set -euo pipefail

case "${0##*/}" in
  wget)
    printf '%s\n' "$*" >> /tmp/wget.log
    url=""
    spider=false
    output="-"
    while (($# > 0)); do
      case "$1" in
        -O)
          (($# >= 2))
          output="$2"
          shift 2
          ;;
        -T)
          (($# >= 2))
          shift 2
          ;;
        -q | -S)
          shift
          ;;
        --spider)
          spider=true
          shift
          ;;
        https://*)
          url="$1"
          shift
          ;;
        *)
          printf 'unsupported wget argument in worker installer fixture: %s\n' "$1" >&2
          exit 1
          ;;
      esac
    done
    [[ -n "$url" ]]
    if [[ "$spider" == "true" ]]; then
      printf '  Location: https://github.com/dimasma0305/rsctf/releases/tag/v0.1.0\n' >&2
    elif [[ "$url" == */healthz ]]; then
      if [[ "${RSCTF_TEST_HEALTHY:-1}" == 1 ]]; then
        if [[ "$output" == "-" ]]; then
          printf 'ok'
        else
          printf 'ok' > "$output"
        fi
      else
        if [[ "$output" == "-" ]]; then
          printf 'unavailable'
        else
          printf 'unavailable' > "$output"
        fi
        exit 8
      fi
    else
      if [[ "$output" == "-" ]]; then
        cat "/fixture/${url##*/}"
      else
        cat "/fixture/${url##*/}" > "$output"
      fi
    fi
    ;;
  docker)
    case "${1:-}" in
      info)
        exit 0
        ;;
      ps)
        [[ "${RSCTF_TEST_MANAGED_CONTAINERS:-0}" == 0 ]] || printf 'managed-container\n'
        exit 0
        ;;
      network)
        [[ "${2:-}" == ls ]] || exit 1
        [[ "${RSCTF_TEST_MANAGED_NETWORKS:-0}" == 0 ]] || printf 'managed-network\n'
        exit 0
        ;;
      volume)
        case "${2:-}" in
          inspect)
            [[ "${RSCTF_TEST_OWNER_VOLUME:-0}" == 0 ]] || printf 'worker-id\n'
            exit 0
            ;;
          rm)
            printf '%s\n' "$*" >> /tmp/docker.log
            exit 0
            ;;
        esac
        ;;
      "")
        exit 0
        ;;
    esac
    exit 1
    ;;
  gh)
    if [[ "$*" == "attestation verify --help" ]]; then
      printf '%s\n' \
        '--bundle' \
        '--deny-self-hosted-runners' \
        '--hostname' \
        '--repo' \
        '--signer-workflow' \
        '--source-ref'
      exit 0
    fi
    if [[ "${1:-}" == attestation && "${2:-}" == verify ]]; then
      printf '%s\n' "$*" >> /tmp/gh.log
      [[ "${RSCTF_TEST_ATTESTATION_SUCCESS:-0}" == 1 ]]
      exit
    fi
    exit 1
    ;;
  systemctl)
    printf '%s\n' "$*" >> /tmp/systemctl.log
    if [[ "${1:-}" == "is-active" ]]; then
      [[ "${RSCTF_TEST_SERVICE_ACTIVE:-0}" == 1 ]]
      exit
    fi
    if [[ "${1:-}" == "is-enabled" ]]; then
      [[ "${RSCTF_TEST_SERVICE_ENABLED:-0}" == 1 ]]
      exit
    fi
    if [[ "${1:-}" == "daemon-reload" ]]; then
      reload_count="$(grep -c '^daemon-reload$' /tmp/systemctl.log || true)"
      ((reload_count > ${RSCTF_TEST_FAIL_DAEMON_RELOADS:-0}))
      exit
    fi
    if [[ "${1:-}" == "enable" ]]; then
      enable_count="$(grep -c '^enable rsctf-worker-agent.service$' /tmp/systemctl.log || true)"
      ((enable_count > ${RSCTF_TEST_FAIL_ENABLES:-0}))
      exit
    fi
    if [[ "${1:-}" == "restart" ]]; then
      restart_count="$(grep -c '^restart rsctf-worker-agent.service$' /tmp/systemctl.log || true)"
      if ((restart_count > ${RSCTF_TEST_FAIL_RESTARTS:-0})); then
        if [[ "${RSCTF_TEST_CONTROL_CONNECTED:-0}" == 1 ]]; then
          mkdir -p /run/rsctf-worker-agent
          printf 'online\n' > /run/rsctf-worker-agent/connected
        fi
        exit 0
      fi
      exit 1
    fi
    if [[ "${1:-}" == "reset-failed" || "${1:-}" == "show" ]]; then
      exit 0
    fi
    ;;
  journalctl)
    printf '%s\n' "$*" >> /tmp/journalctl.log
    if [[ "${RSCTF_TEST_CONTROL_CONNECTED:-0}" == 1 ]]; then
      printf 'worker control session established\n'
    else
      printf '%s\n' \
        "${RSCTF_TEST_WORKER_DIAGNOSTIC:-worker control session failed: fixture unavailable}"
    fi
    ;;
  sudo)
    printf '%s\n' "$*" > /tmp/sudo.log
    exit 73
    ;;
  *)
    printf 'unsupported worker installer test shim command: %s\n' "${0##*/}" >&2
    exit 1
    ;;
esac
