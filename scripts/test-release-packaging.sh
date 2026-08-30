#!/usr/bin/env bash
set -euo pipefail

temporary_directory="$(mktemp -d)"
trap 'rm -rf "${temporary_directory}"' EXIT
fixtures="${temporary_directory}/fixtures"
artifacts="${temporary_directory}/artifacts"
install_root="${temporary_directory}/install"
agent_home="${temporary_directory}/agent-home"
mkdir -p "${fixtures}" "${artifacts}" "${agent_home}"
printf 'keep\n' >"${agent_home}/sentinel"

for binary in lenso-agent lenso-agent-cli lenso-agent-acp; do
  cp scripts/check-product-identity.sh "${fixtures}/${binary}"
  chmod 0755 "${fixtures}/${binary}"
  ./scripts/package-release-binary.sh \
    0.1.0 "${binary}" darwin-aarch64 "${fixtures}/${binary}" "${artifacts}" >/dev/null
done

first_archive_digest="$(shasum -a 256 "${artifacts}/lenso-agent-v0.1.0-darwin-aarch64.tar.gz" | awk '{print $1}')"
./scripts/package-release-binary.sh \
  0.1.0 lenso-agent darwin-aarch64 "${fixtures}/lenso-agent" "${artifacts}" >/dev/null
second_archive_digest="$(shasum -a 256 "${artifacts}/lenso-agent-v0.1.0-darwin-aarch64.tar.gz" | awk '{print $1}')"
test "${first_archive_digest}" = "${second_archive_digest}"

base_url="file://${artifacts}"
if ./scripts/install.sh \
  --version 0.1.0 \
  --target darwin-x86_64 \
  --base-url "${base_url}" \
  --install-dir "${install_root}/unsupported" >/dev/null 2>&1; then
  echo "error: installer accepted an unpublished release target" >&2
  exit 1
fi

fake_system="${temporary_directory}/fake-system"
mkdir -p "${fake_system}"
printf '#!/bin/sh\nprintf "Darwin\\n"\n' >"${fake_system}/uname"
printf '#!/bin/sh\nprintf "14.7.0\\n"\n' >"${fake_system}/sw_vers"
chmod 0755 "${fake_system}/uname" "${fake_system}/sw_vers"
if PATH="${fake_system}:${PATH}" ./scripts/install.sh \
  --version 0.1.0 \
  --target darwin-aarch64 \
  --base-url "${base_url}" \
  --install-dir "${install_root}/old-macos" >/dev/null 2>&1; then
  echo "error: installer accepted macOS older than 15" >&2
  exit 1
fi

LENSO_AGENT_HOME="${agent_home}" ./scripts/install.sh \
  --version 0.1.0 \
  --target darwin-aarch64 \
  --base-url "${base_url}" \
  --install-dir "${install_root}/bin" \
  --component agent \
  --component cli \
  --component acp >/dev/null
for binary in lenso-agent lenso-agent-cli lenso-agent-acp; do
  test -x "${install_root}/bin/${binary}"
done


LENSO_AGENT_HOME="${agent_home}" ./scripts/install.sh \
  --version 0.1.0 \
  --target darwin-aarch64 \
  --base-url "${base_url}" \
  --install-dir "${install_root}/bin" \
  --component agent \
  --component cli \
  --component acp >/dev/null

before="$(shasum -a 256 "${install_root}/bin/lenso-agent" | awk '{print $1}')"
printf 'tamper\n' >>"${artifacts}/lenso-agent-v0.1.0-darwin-aarch64.tar.gz"
if LENSO_AGENT_HOME="${agent_home}" ./scripts/install.sh \
  --version 0.1.0 \
  --target darwin-aarch64 \
  --base-url "${base_url}" \
  --install-dir "${install_root}/bin" \
  --component agent >/dev/null 2>&1; then
  echo "error: installer accepted a tampered archive" >&2
  exit 1
fi
after="$(shasum -a 256 "${install_root}/bin/lenso-agent" | awk '{print $1}')"
test "${before}" = "${after}"

LENSO_AGENT_HOME="${agent_home}" ./scripts/install.sh \
  --install-dir "${install_root}/bin" \
  --component agent \
  --component cli \
  --component acp \
  --uninstall >/dev/null
test -f "${agent_home}/sentinel"
for binary in lenso-agent lenso-agent-cli lenso-agent-acp; do
  test ! -e "${install_root}/bin/${binary}"
done

LENSO_AGENT_HOME="${agent_home}" ./scripts/install.sh \
  --install-dir "${install_root}/bin" \
  --uninstall \
  --purge-agent-home >/dev/null
test ! -e "${agent_home}"

if LENSO_AGENT_HOME="${HOME}/.." ./scripts/install.sh \
  --install-dir "${install_root}/bin" \
  --uninstall \
  --purge-agent-home >/dev/null 2>&1; then
  echo "error: installer accepted a broad Agent Home purge target" >&2
  exit 1
fi
test -d "${temporary_directory}"
echo "release packaging and lifecycle checks passed"
