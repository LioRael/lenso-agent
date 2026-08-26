#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lenso_bin="${LENSO_BIN:-lenso}"
cd "${repo_root}"
mkdir -p .lenso
proof_root="$(mktemp -d .lenso/removal-proof.XXXXXX)"
trap 'rm -rf "${proof_root}"' EXIT

"${lenso_bin}" app check --definition lenso.app.json

remove_modules() {
  local source="$1"
  local target="$2"
  shift 2
  local keys
  keys="$(printf '%s\n' "$@" | jq -R . | jq -s .)"
  jq --argjson keys "${keys}" \
    '.manifest = "../../Cargo.toml" |
      .app.modules |= map(select(.key as $key | ($keys | index($key) | not))) |
      .app.binding_policies |= map(select(
        (.consumer as $consumer | ($keys | index($consumer) | not)) and
        (.provider as $provider | ($keys | index($provider) | not))
      )) |
      .app.decisions |= map(select(
        (.consumer as $consumer | ($keys | index($consumer) | not)) and
        (.provider as $provider | ($keys | index($provider) | not))
      ))' \
    "${source}" > "${target}"
  "${lenso_bin}" app check --definition "${target}"
}

remove_modules lenso.app.json \
  "${proof_root}/headless-without-workspace-read.app.json" \
  workspace-read
remove_modules lenso.app.json \
  "${proof_root}/headless-without-prompt-providers.app.json" \
  fixture-instructions summary-skill
remove_modules lenso.app.json \
  "${proof_root}/tui-without-panels.app.json" \
  tui-help
remove_modules lenso.app.json \
  "${proof_root}/headless-without-http-fetch.app.json" \
  http-fetch
remove_modules lenso.app.json \
  "${proof_root}/without-telegram-surface.app.json" \
  telegram
remove_modules lenso.app.json \
  "${proof_root}/without-discord-surface.app.json" \
  discord
