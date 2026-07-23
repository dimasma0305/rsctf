#!/usr/bin/env bash

set -euo pipefail

case "${0##*/}" in
  curl)
    printf '%s\n' "$*" >> /tmp/curl.log
    destination=""
    url=""
    while (($# > 0)); do
      case "$1" in
        --output)
          (($# >= 2))
          destination="$2"
          shift 2
          ;;
        --connect-timeout | --max-filesize | --max-time | --proto | --proto-redir | \
          --retry | --retry-delay | --retry-max-time | --speed-limit | --speed-time | \
          --write-out)
          (($# >= 2))
          shift 2
          ;;
        --disable | --fail | --location | --retry-all-errors | --show-error | --silent | --tlsv1.2)
          shift
          ;;
        https://*)
          url="$1"
          shift
          ;;
        *)
          printf 'unsupported curl argument in worker installer fixture: %s\n' "$1" >&2
          exit 1
          ;;
      esac
    done
    [[ -n "$destination" && -n "$url" ]]
    cp "/fixture/${url##*/}" "$destination"
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
      ((restart_count > ${RSCTF_TEST_FAIL_RESTARTS:-0}))
      exit
    fi
    ;;
  *)
    printf 'unsupported worker installer test shim command: %s\n' "${0##*/}" >&2
    exit 1
    ;;
esac
