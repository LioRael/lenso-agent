#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: package-acp-binary.sh <version> <registry-target> <binary> <output-directory>" >&2
  exit 2
}

[[ $# -eq 4 ]] || usage

version="$1"
registry_target="$2"
binary="$3"
output_directory="$4"

[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "error: version must be an exact semantic version" >&2
  exit 2
}

case "${registry_target}" in
  darwin-aarch64|darwin-x86_64|linux-aarch64|linux-x86_64|windows-aarch64|windows-x86_64) ;;
  *)
    echo "error: unsupported ACP Registry target: ${registry_target}" >&2
    exit 2
    ;;
esac

[[ -f "${binary}" ]] || {
  echo "error: ACP binary does not exist: ${binary}" >&2
  exit 2
}

mkdir -p "${output_directory}"
temporary_directory="$(mktemp -d)"
trap 'rm -rf "${temporary_directory}"' EXIT

executable_name="lenso-agent-acp"
if [[ "${registry_target}" == windows-* ]]; then
  executable_name="lenso-agent-acp.exe"
fi
install -m 0755 "${binary}" "${temporary_directory}/${executable_name}"
TZ=UTC touch -t 198001010000 "${temporary_directory}/${executable_name}"

archive_name="lenso-agent-acp-v${version}-${registry_target}.tar.gz"
archive_path="${output_directory}/${archive_name}"
tar --format ustar --uid 0 --gid 0 --numeric-owner \
  -C "${temporary_directory}" -cf - "${executable_name}" |
  gzip -n -9 >"${archive_path}"

checksum="$(shasum -a 256 "${archive_path}" | awk '{print $1}')"
printf '%s  %s\n' "${checksum}" "${archive_name}" >"${archive_path}.sha256"
printf '%s\n' "${archive_path}"
