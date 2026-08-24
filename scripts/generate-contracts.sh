#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract_codegen="${LENSO_CONTRACT_CODEGEN:-lenso-contract-codegen}"

contract_roots=(
  "${repo_root}/crates/lenso-capability-agent"
  "${repo_root}/crates/lenso-capability-agent-auth-openai-codex"
  "${repo_root}/crates/lenso-capability-agent-model"
  "${repo_root}/crates/lenso-capability-agent-prompt"
  "${repo_root}/crates/lenso-capability-agent-prompt-provider"
  "${repo_root}/crates/lenso-capability-agent-process"
  "${repo_root}/crates/lenso-capability-agent-tools"
  "${repo_root}/crates/lenso-capability-agent-tool-provider"
  "${repo_root}/crates/lenso-capability-agent-session"
)

for contract_root in "${contract_roots[@]}"; do
  "${contract_codegen}" generate \
    "${contract_root}/capability.json" \
    --rust \
    "${contract_root}/src/generated.rs"
done
