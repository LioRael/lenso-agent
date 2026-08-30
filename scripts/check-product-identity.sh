#!/usr/bin/env bash
set -euo pipefail

former_repository='lenso-agent-''harness'
former_product='Lenso Agent ''Harness'

if identity_matches="$(
  git grep -n -I -E "${former_repository}|${former_product}" -- . \
    ':!docs/adr/**' \
    ':!docs/evidence/**' \
    ':!docs/research/**'
)"; then
  echo "error: active product files still use the former Lenso Agent identity" >&2
  printf '%s\n' "${identity_matches}" >&2
  exit 1
fi
