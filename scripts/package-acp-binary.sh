#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: package-acp-binary.sh <version> <registry-target> <binary> <output-directory>" >&2
  exit 2
}

[[ $# -eq 4 ]] || usage

exec "$(dirname "$0")/package-release-binary.sh" \
  "$1" lenso-agent-acp "$2" "$3" "$4"
