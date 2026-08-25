#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract_codegen="${LENSO_CONTRACT_CODEGEN:-lenso-contract-codegen}"

contracts=(
  "lenso-capability-agent:rust-runtime"
  "lenso-capability-agent-auth-openai-codex:rust"
  "lenso-capability-agent-model:rust-runtime"
  "lenso-capability-agent-prompt:rust-runtime"
  "lenso-capability-agent-prompt-provider:rust"
  "lenso-capability-agent-process:rust"
  "lenso-capability-agent-tools:rust-runtime"
  "lenso-capability-agent-tool-provider:rust-runtime"
  "lenso-capability-agent-session:rust-runtime"
)

for contract in "${contracts[@]}"; do
  crate="${contract%%:*}"
  projection="${contract##*:}"
  contract_root="${repo_root}/crates/${crate}"
  "${contract_codegen}" generate \
    "${contract_root}/capability.json" \
    "--${projection}" \
    "${contract_root}/src/generated.rs"
done
