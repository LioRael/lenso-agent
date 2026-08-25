#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lenso_bin="${LENSO_BIN:-lenso}"
cd "${repo_root}"
proof_root="$(mktemp -d composition/.removal-proof.XXXXXX)"
trap 'rm -rf "${proof_root}"' EXIT

for definition in composition/*.app.json; do
  "${lenso_bin}" app check --definition "${definition}"
done

remove_modules() {
  local source="$1"
  local target="$2"
  shift 2
  local keys
  keys="$(printf '%s\n' "$@" | jq -R . | jq -s .)"
  jq --argjson keys "${keys}" \
    '.manifest = "../../Cargo.toml" |
      .app.modules |= map(select(.key as $key | ($keys | index($key) | not)))' \
    "${source}" > "${target}"
  "${lenso_bin}" app check --definition "${target}"
}

remove_modules composition/headless-readonly.app.json \
  "${proof_root}/headless-without-workspace-read.app.json" \
  workspace-read
remove_modules composition/headless-readonly.app.json \
  "${proof_root}/headless-without-prompt-providers.app.json" \
  fixture-instructions summary-skill
remove_modules composition/tui-readonly.app.json \
  "${proof_root}/tui-without-panels.app.json" \
  tui-help
remove_modules composition/openai-codex-direct-skills.app.json \
  "${proof_root}/codex-without-skills.app.json" \
  skills
remove_modules composition/headless-coding.app.json \
  "${proof_root}/headless-without-workspace-edit.app.json" \
  workspace-edit
remove_modules composition/headless-local-coding.app.json \
  "${proof_root}/headless-without-process.app.json" \
  process-tools native-process
