#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: package-release-binary.sh <version> <executable-name> <release-target> <binary> <output-directory>" >&2
  exit 2
}

[[ $# -eq 5 ]] || usage

version="$1"
executable_name="$2"
release_target="$3"
binary="$4"
output_directory="$5"

[[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "error: version must be an exact semantic version" >&2
  exit 2
}
[[ "${executable_name}" =~ ^lenso-agent(-[a-z0-9-]+)?$ ]] || {
  echo "error: unsupported release executable: ${executable_name}" >&2
  exit 2
}
case "${release_target}" in
  darwin-aarch64|darwin-x86_64|linux-aarch64|linux-x86_64|windows-aarch64|windows-x86_64) ;;
  *)
    echo "error: unsupported release target: ${release_target}" >&2
    exit 2
    ;;
esac
[[ -f "${binary}" ]] || {
  echo "error: release binary does not exist: ${binary}" >&2
  exit 2
}

mkdir -p "${output_directory}"
temporary_directory="$(mktemp -d)"
trap 'rm -rf "${temporary_directory}"' EXIT

archive_executable="${executable_name}"
if [[ "${release_target}" == windows-* ]]; then
  archive_executable="${archive_executable}.exe"
fi
install -m 0755 "${binary}" "${temporary_directory}/${archive_executable}"
TZ=UTC touch -t 198001010000 "${temporary_directory}/${archive_executable}"

archive_name="${executable_name}-v${version}-${release_target}.tar.gz"
archive_path="${output_directory}/${archive_name}"
tar --format ustar --uid 0 --gid 0 --numeric-owner \
  -C "${temporary_directory}" -cf - "${archive_executable}" |
  gzip -n -9 >"${archive_path}"

if command -v shasum >/dev/null 2>&1; then
  checksum="$(shasum -a 256 "${archive_path}" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
  checksum="$(sha256sum "${archive_path}" | awk '{print $1}')"
else
  echo "error: shasum or sha256sum is required" >&2
  exit 1
fi
printf '%s  %s\n' "${checksum}" "${archive_name}" >"${archive_path}.sha256"
printf '%s\n' "${archive_path}"
