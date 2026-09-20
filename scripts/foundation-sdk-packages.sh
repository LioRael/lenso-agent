#!/usr/bin/env bash
# Closed release cohort: never publish the entire Agent workspace.
set -euo pipefail
packages=(
  lenso-capability-agent-model
  lenso-capability-agent-turn-processing
  lenso-capability-agent-extension-state
  lenso-capability-agent-interaction
  lenso-capability-agent-dynamic-authority
  lenso-capability-agent-durable-task
)
args=()
for package in "${packages[@]}"; do
  args+=(-p "$package")
done
case "${1:-}" in
  test) cargo test --locked "${args[@]}" ;;
  verify) cargo publish --dry-run "${args[@]}" ;;
  publish) cargo publish "${args[@]}" ;;
  *) echo 'usage: foundation-sdk-packages.sh test|verify|publish' >&2; exit 2 ;;
esac
