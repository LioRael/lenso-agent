#!/usr/bin/env bash
set -euo pipefail

[[ $# -eq 1 ]] || {
  echo "usage: check-release-version.sh <version>" >&2
  exit 2
}
version="$1"
[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "error: version must be an exact semantic version" >&2
  exit 2
}

for manifest in apps/lenso-agent-{tui,cli,acp}/Cargo.toml; do
  manifest_version="$(awk -F ' *= *' '$1 == "version" { gsub(/"/, "", $2); print $2; exit }' "${manifest}")"
  [[ "${manifest_version}" == "${version}" ]] || {
    echo "error: ${manifest} is ${manifest_version}; expected ${version}" >&2
    exit 1
  }
done
installer_version="$(sed -n 's/^DEFAULT_VERSION="\([^"]*\)"/\1/p' scripts/install.sh)"
[[ "${installer_version}" == "${version}" ]] || {
  echo "error: scripts/install.sh defaults to ${installer_version}; expected ${version}" >&2
  exit 1
}
echo "release version is consistent: ${version}"
