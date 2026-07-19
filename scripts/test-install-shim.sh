#!/usr/bin/env bash

set -euo pipefail

case "${0##*/}" in
  curl)
    printf '%s\n' "$*" >> "${RSCTF_TEST_CURL_LOG:?}"
    destination=""
    write_out=""
    url=""
    while (($#)); do
      case "$1" in
        --output)
          destination=$2
          shift 2
          ;;
        --write-out)
          write_out=$2
          shift 2
          ;;
        --connect-timeout | --max-filesize | --max-time | --proto | --proto-redir | \
          --retry | --retry-delay | --retry-max-time | --speed-limit | --speed-time)
          shift 2
          ;;
        --disable | --fail | --location | --retry-all-errors | --show-error | --silent | --tlsv1.2)
          shift
          ;;
        https://*)
          url=$1
          shift
          ;;
        *)
          printf 'unsupported installer curl argument: %s\n' "$1" >&2
          exit 1
          ;;
      esac
    done
    [[ -n "$url" ]]
    if [[ -n "$write_out" ]]; then
      [[ "$write_out" == '%{url_effective}' && "$destination" == /dev/null ]]
      printf 'https://github.com/dimasma0305/rsctf/releases/tag/%s' \
        "${RSCTF_TEST_LATEST_TAG:-v1.2.3}"
    else
      [[ -n "$destination" ]]
      cp "${RSCTF_INSTALLER_FIXTURE:?}/${url##*/}" "$destination"
    fi
    ;;
  docker)
    if [[ "$*" == *"config --services"* ]]; then
      printf 'db\nredis\nrsctf\n'
    fi
    ;;
  gh)
    if [[ "$*" == "attestation verify --help" ]]; then
      printf '%s\n' \
        --bundle \
        --deny-self-hosted-runners \
        --hostname \
        --repo \
        --signer-workflow \
        --source-ref
      exit
    fi
    if [[ "${1:-}" == attestation && "${2:-}" == verify ]]; then
      printf '%s\n' "$*" >> "${RSCTF_TEST_GH_LOG:?}"
      [[ "${RSCTF_TEST_GH_VERIFY:-1}" == 1 ]]
      exit
    fi
    exit 1
    ;;
  *)
    printf 'unsupported installer fixture command: %s\n' "${0##*/}" >&2
    exit 1
    ;;
esac
