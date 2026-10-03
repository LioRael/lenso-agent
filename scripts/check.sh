#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$root"
export RUSTUP_TOOLCHAIN=nightly-2026-07-10
export PYTHONDONTWRITEBYTECODE=1
phase="${1:-all}"
case "$phase" in all|preflight|static|tests|contracts|foundation-targets|marketplace-installation) ;; *) echo "Unknown check phase: $phase" >&2; exit 2 ;; esac
printf 'Agent check: sha=%s phase=%s os=%s\n' "$(git rev-parse HEAD)" "$phase" "$(uname -sm)"
git status --short
rustc --version
cargo --version
run() { printf '+ '; printf '%q ' "$@"; printf '\n'; "$@"; }
preflight() {
  local former_repository='lenso-agent-''harness'
  local former_product='Lenso Agent ''Harness'
  if git grep -n -I -E "${former_repository}|${former_product}" -- . ':!docs/adr/**' ':!docs/research/**'; then
    echo "active product files still use the former Lenso Agent identity" >&2; return 1
  fi
  # Syntax and cohort versions need no compilation or registry access.
  for script in scripts/*.sh; do run bash -n "$script"; done
  local release_version
  release_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' apps/lenso-agent-tui/Cargo.toml | head -n 1)"
  run bash scripts/check-release-version.sh "$release_version"
  run bash scripts/test-release-packaging.sh
  run python3 scripts/check-fixture-inputs.py
  run python3 scripts/test-check-fixture-inputs.py
  run python3 scripts/test-console-profile-admission.py
  # Pure offline regressions; real cross-repository readiness needs explicit
  # reviewed checkouts/artifacts via check-version-consistency.py.
  run python3 scripts/test-tool-starter-prerequisites.py
  run python3 scripts/test-version-consistency.py
  run python3 scripts/test-version-manifest-inputs.py
  run python3 scripts/test-version-doc-entrypoints.py
  run python3 scripts/test-check-agent-foundation-lifecycle.py
}
static() {
  preflight
  run cargo fmt --all -- --check
  run cargo clippy --locked --workspace --all-targets -- -D warnings
}
tests() {
  test "$(bun --version)" = 1.4.0
  if [[ "$(uname -s)" == Linux ]]; then
    run bwrap --ro-bind / / --unshare-net /bin/true
    run rg --version
  fi
  # Test the linked optional inventory before the entire workspace suite.
  run cargo test --locked -p lenso-agent-dialogue-starter --lib
  run cargo test --locked -p lenso-agent-foundation --lib
  run cargo test --locked --workspace --all-targets
  (cd packages/lenso-agent-tool-sdk; run bun install --frozen-lockfile; run bun run build; run bun test test)
  (cd packages/agent-tool-convention; run bun install --frozen-lockfile; run bun run check)
  run cargo build --locked -p lenso-agent-web --no-default-features --features console-standalone --bin lenso-agent-console-web
  run python3 scripts/check-console-profile-admission.py "${CARGO_TARGET_DIR:-target}/debug/lenso-agent-console-web"
}
contracts() {
  run cargo install lenso-contract-codegen --version '=0.10.0' --locked
  run lenso-contract-codegen workspace check --manifest-path Cargo.toml
  run python3 examples/external-task-board-capability/verify-registry.py
  run cargo package -p lenso-capability-agent-durable-task -p lenso-capability-agent-extension-state
  run python3 examples/external-agent-durable-state/verify-artifacts.py --artifacts-dir "${CARGO_TARGET_DIR:-target}/package"
}
foundation_targets() {
  run python3 examples/external-agent-targets/verify.py
  run python3 examples/external-agent-durable-state/verify-binary-upgrade.py
  run python3 scripts/check-agent-foundation-lifecycle.py
}
marketplace_installation() (
  run cargo install lenso-cli --version 0.6.3 --locked
  local fixture
  fixture="$(mktemp -d "${TMPDIR:-/tmp}/lenso-marketplace-proof.XXXXXX")"
  trap 'rm -rf -- "$fixture"' EXIT
  CARGO=cargo run python3 apps/lenso-agent-web/tests/fixtures/marketplace-proof/build-variants.py "$fixture"
  export LENSO_MARKETPLACE_ARCHIVE="$fixture/proof-0.1.0.lenso-plugin"
  export LENSO_MARKETPLACE_UPDATE_ARCHIVE="$fixture/proof-0.2.0.lenso-plugin"
  export LENSO_MARKETPLACE_FAILURE_ARCHIVE="$fixture/proof-0.3.0.lenso-plugin"
  run cargo test --locked -p lenso-agent-web --lib console_tools_install_signed_release_and_observe_runtime -- --ignored
)
case "$phase" in
  all) test "$(bun --version)" = 1.4.0; static; tests; contracts; foundation_targets; marketplace_installation ;;
  foundation-targets) foundation_targets ;;
  marketplace-installation) marketplace_installation ;;
  *) "$phase" ;;
esac
