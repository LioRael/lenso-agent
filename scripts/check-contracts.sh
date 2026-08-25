#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract_codegen="${LENSO_CONTRACT_CODEGEN:-lenso-contract-codegen}"

contract_codegen_usage="$("${contract_codegen}" 2>&1 || true)"
if ! grep -q 'lenso-contract-codegen workspace' <<<"${contract_codegen_usage}"; then
  printf '%s\n' \
    "error: ${contract_codegen} does not support workspace contract commands" \
    'install lenso-contract-codegen from the revision pinned in Cargo.toml,' \
    'or set LENSO_CONTRACT_CODEGEN to that executable' >&2
  exit 2
fi

"${contract_codegen}" workspace check --manifest-path "${repo_root}/Cargo.toml"
