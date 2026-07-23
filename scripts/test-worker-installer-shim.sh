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
    readonly test_image_id="sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    printf '%s\n' "$*" >> /tmp/docker.log
    case "${1:-}" in
      info)
        case "$*" in
          *'{{.OSType}}'*) printf 'linux\n' ;;
          *'{{.Architecture}}'*) printf 'amd64\n' ;;
          *'{{.DockerRootDir}}'*) printf '/var/lib/docker\n' ;;
        esac
        exit 0
        ;;
      ps)
        if [[ "$*" == *"io.rsctf.worker.managed=true"* ]]; then
          [[ "${RSCTF_TEST_MANAGED_CONTAINERS:-0}" == 0 ]] ||
            printf 'managed-container\n'
        elif [[ "$*" == *"name=^/rsctf-worker-agent$"* ]]; then
          printf 'rsctf-worker-agent\n'
        fi
        exit 0
        ;;
      run)
        if [[ "$*" == *" installation-status --state-dir "* ]]; then
          printf '%s\n' "${RSCTF_TEST_INSTALLATION_STATUS:-empty}"
          exit 0
        fi
        if [[ "$*" == *" doctor" ]]; then
          [[ "${RSCTF_TEST_DOCTOR_SUCCESS:-1}" == 1 ]]
          exit
        fi
        if [[ "$*" == *" enroll --server-url "* ]]; then
          [[ "${RSCTF_TEST_ENROLL_SUCCESS:-1}" == 1 ]]
          exit
        fi
        if [[ "$*" == *"--detach"* && "$*" == *" run --config "* ]]; then
          touch /tmp/rsctf-test-container-rsctf-worker-agent
          printf 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n'
          exit 0
        fi
        exit 1
        ;;
      import)
        printf '%s\n' "$test_image_id"
        exit 0
        ;;
      image)
        case "${2:-}" in
          inspect)
            printf '%s\n' "$test_image_id"
            exit 0
            ;;
          ls)
            [[ "${RSCTF_TEST_WORKER_IMAGES:-0}" == 0 ]] ||
              printf '%s\n' "$test_image_id"
            exit 0
            ;;
          rm)
            exit 0
            ;;
        esac
        exit 1
        ;;
      container)
        case "${2:-}" in
          inspect)
            container_name="${!#}"
            marker="/tmp/rsctf-test-container-${container_name}"
            if [[ ! -f "$marker" &&
              ! ("$container_name" == rsctf-worker-agent &&
                "${RSCTF_TEST_EXISTING_AGENT_CONTAINER:-0}" == 1) ]]; then
              exit 1
            fi
            case "$*" in
              *'io.rsctf.worker.agent'*)
                printf '%s\n' "${RSCTF_TEST_AGENT_LABEL:-true}"
                ;;
              *'{{.State.Running}}'*)
                [[ "${RSCTF_TEST_CONTAINER_RUNNING:-1}" == 1 ]] &&
                  printf 'true\n' || printf 'false\n'
                ;;
              *'restartCount='*)
                printf 'name=/%s running=%s status=running restartCount=0 image=%s\n' \
                  "$container_name" \
                  "$([[ "${RSCTF_TEST_CONTAINER_RUNNING:-1}" == 1 ]] &&
                    printf true || printf false)" \
                  "$test_image_id"
                ;;
            esac
            exit 0
            ;;
          rm)
            container_name="${!#}"
            rm -f "/tmp/rsctf-test-container-${container_name}"
            exit 0
            ;;
        esac
        exit 1
        ;;
      cp)
        if [[ "${RSCTF_TEST_CONTROL_CONNECTED:-0}" == 1 ]]; then
          destination="${!#}"
          printf 'online\n' > "$destination"
          exit 0
        fi
        exit 1
        ;;
      logs)
        printf '%s\n' \
          "${RSCTF_TEST_WORKER_DIAGNOSTIC:-worker control session failed: fixture unavailable}"
        exit 0
        ;;
      stop)
        exit 0
        ;;
      rename)
        old_name="${2:-}"
        new_name="${3:-}"
        old_marker="/tmp/rsctf-test-container-${old_name}"
        new_marker="/tmp/rsctf-test-container-${new_name}"
        if [[ -f "$old_marker" ]]; then
          mv "$old_marker" "$new_marker"
        else
          touch "$new_marker"
        fi
        exit 0
        ;;
      start)
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
            volume_name="${!#}"
            if [[ "$volume_name" == rsctf-worker-owner &&
              "${RSCTF_TEST_OWNER_VOLUME:-0}" == 1 ]]; then
              printf 'worker-id\n'
              exit 0
            fi
            if [[ "$volume_name" == rsctf-worker-state &&
              (-f /tmp/rsctf-test-worker-state-volume ||
                "${RSCTF_TEST_STATE_VOLUME:-0}" == 1) ]]; then
              printf '%s\n' "${RSCTF_TEST_STATE_LABEL:-true}"
              exit 0
            fi
            exit 1
            ;;
          create)
            touch /tmp/rsctf-test-worker-state-volume
            printf 'rsctf-worker-state\n'
            exit 0
            ;;
          rm)
            rm -f /tmp/rsctf-test-worker-state-volume
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
