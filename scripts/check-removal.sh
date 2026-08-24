#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lenso_bin="${LENSO_BIN:-lenso}"
recipe="composition/recipes.json"
execution_class="lenso.native-rust@1"

cd "${repo_root}"
"${lenso_bin}" compose check --recipe "${recipe}" \
  --execution-class "${execution_class}"
"${lenso_bin}" compose check --recipe "${recipe}" \
  --variant headless-readonly \
  --without composition/fragments/tools/workspace-read.json \
  --execution-class "${execution_class}"
"${lenso_bin}" compose check --recipe "${recipe}" \
  --variant headless-readonly \
  --without composition/fragments/prompt/fixture.json \
  --without composition/fragments/prompt/summary.json \
  --execution-class "${execution_class}"
"${lenso_bin}" compose check --recipe "${recipe}" \
  --variant openai-codex-direct-skills \
  --without composition/fragments/tools/skills.json \
  --execution-class "${execution_class}"
"${lenso_bin}" compose check --recipe "${recipe}" \
  --variant headless-coding \
  --without composition/fragments/tools/coding.json \
  --execution-class "${execution_class}"
"${lenso_bin}" compose check --recipe "${recipe}" \
  --variant headless-local-coding \
  --without composition/fragments/tools/process.json \
  --without composition/fragments/process/fixture.json \
  --execution-class "${execution_class}"
