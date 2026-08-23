#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract_codegen="${LENSO_CONTRACT_CODEGEN:-lenso-contract-codegen}"

contracts=(
  lenso-capability-agent
  lenso-capability-agent-model
  lenso-capability-agent-tools
  lenso-capability-agent-tool-provider
  lenso-capability-agent-session
)

for contract in "${contracts[@]}"; do
  contract_root="${repo_root}/crates/${contract}"
  "${contract_codegen}" generate \
    "${contract_root}/capability.json" \
    "${contract_root}/src/generated.rs" \
    "${contract_root}/generated/bindings.ts"
done
