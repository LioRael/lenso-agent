#!/bin/sh
set -eu

DEFAULT_VERSION="0.1.1"
REPOSITORY="LioRael/lenso-agent"

usage() {
  cat <<'EOF'
Install or remove Lenso Agent release binaries.

usage: install.sh [options]

  --version <x.y.z>          Exact release version (default: 0.1.1)
  --install-dir <absolute>   Binary directory (default: ~/.local/bin)
  --component <name>         agent, cli, web, console-web, or acp; repeatable
  --target <platform>        Override detected release target
  --base-url <URL>           Override the exact-version asset base URL
  --uninstall                Remove selected binaries and preserve Agent Home
  --purge-agent-home         With --uninstall, also remove LENSO_AGENT_HOME
  --help                     Show this help

The default installation contains the interactive Agent, management CLI, and
both loopback Web APIs. ACP remains an independent optional distribution.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 1
}

version="${LENSO_AGENT_VERSION:-$DEFAULT_VERSION}"
install_dir="${LENSO_AGENT_INSTALL_DIR:-${HOME}/.local/bin}"
base_url=""
release_target=""
components=""
uninstall=false
purge_agent_home=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || fail "--version requires a value"
      version="$2"
      shift 2
      ;;
    --install-dir)
      [ "$#" -ge 2 ] || fail "--install-dir requires a value"
      install_dir="$2"
      shift 2
      ;;
    --component)
      [ "$#" -ge 2 ] || fail "--component requires agent, cli, web, console-web, or acp"
      components="${components} $2"
      shift 2
      ;;
    --target)
      [ "$#" -ge 2 ] || fail "--target requires a release target"
      release_target="$2"
      shift 2
      ;;
    --base-url)
      [ "$#" -ge 2 ] || fail "--base-url requires a URL"
      base_url="$2"
      shift 2
      ;;
    --uninstall)
      uninstall=true
      shift
      ;;
    --purge-agent-home)
      purge_agent_home=true
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *) fail "unknown argument: $1" ;;
  esac
done

if [ "$purge_agent_home" = true ] && [ "$uninstall" = false ]; then
  fail "--purge-agent-home requires --uninstall"
fi

case "$version" in
  ''|*[!0-9.]*|.*|*..*|*.) fail "version must be an exact semantic version" ;;
esac
old_ifs="$IFS"
IFS=.
set -- $version
IFS="$old_ifs"
[ "$#" -eq 3 ] || fail "version must be an exact semantic version"

case "$install_dir" in
  /*) ;;
  *) fail "install directory must be absolute: $install_dir" ;;
esac
[ "$install_dir" != "/" ] || fail "refusing to use / as the install directory"

[ -n "${components# }" ] || components=" agent cli web console-web"
for component in $components; do
  case "$component" in
    agent|cli|web|console-web|acp) ;;
    *) fail "unsupported component: $component" ;;
  esac
done

binary_for_component() {
  case "$1" in
    agent) echo "lenso-agent" ;;
    cli) echo "lenso-agent-cli" ;;
    web) echo "lenso-agent-web" ;;
    console-web) echo "lenso-agent-console-web" ;;
    acp) echo "lenso-agent-acp" ;;
  esac
}

if [ "$uninstall" = true ]; then
  for component in $components; do
    binary="$(binary_for_component "$component")"
    rm -f "${install_dir}/${binary}" "${install_dir}/${binary}.exe"
    echo "removed: ${install_dir}/${binary}"
  done
  if [ "$purge_agent_home" = true ]; then
    agent_home="${LENSO_AGENT_HOME:-${HOME}/.lenso/agent}"
    [ -d "$agent_home" ] || fail "Agent Home is not a directory: $agent_home"
    resolved_agent_home="$(cd "$agent_home" && pwd -P)"
    resolved_user_home="$(cd "$HOME" && pwd -P)"
    case "$resolved_agent_home" in
      /|"$resolved_user_home"|/Applications|/Library|/System|/Users|/Volumes|/bin|/dev|/etc|/home|/opt|/private|/proc|/root|/run|/sbin|/srv|/tmp|/usr|/var)
        fail "refusing to purge unsafe Agent Home: $resolved_agent_home"
        ;;
    esac
    rm -rf "$resolved_agent_home"
    echo "removed Agent Home: $resolved_agent_home"
  else
    echo "preserved Agent Home: ${LENSO_AGENT_HOME:-${HOME}/.lenso/agent}"
  fi
  exit 0
fi

operating_system="$(uname -s)"
if [ "$operating_system" = "Darwin" ]; then
  macos_version="$(sw_vers -productVersion)"
  macos_major="${macos_version%%.*}"
  case "$macos_major" in
    ''|*[!0-9]*) fail "could not determine the macOS version: $macos_version" ;;
  esac
  [ "$macos_major" -ge 15 ] || fail "macOS 15 or later is required; found $macos_version"
fi

if [ -z "$release_target" ]; then
  architecture="$(uname -m)"
  case "${operating_system}-${architecture}" in
    Darwin-arm64) release_target="darwin-aarch64" ;;
    Darwin-x86_64) release_target="darwin-x86_64" ;;
    Linux-aarch64|Linux-arm64) release_target="linux-aarch64" ;;
    Linux-x86_64|Linux-amd64) release_target="linux-x86_64" ;;
    *) fail "unsupported platform: ${operating_system}-${architecture}" ;;
  esac
fi
case "$release_target" in
  darwin-aarch64|linux-x86_64) ;;
  *) fail "unsupported release target: $release_target" ;;
esac

[ -n "$base_url" ] || base_url="https://github.com/${REPOSITORY}/releases/download/v${version}"
case "$base_url" in
  https://*|file://*) ;;
  *) fail "release base URL must use HTTPS or file://" ;;
esac

temporary_directory="$(mktemp -d)"
trap 'rm -rf "$temporary_directory"' EXIT HUP INT TERM
mkdir -p "$install_dir"

download() {
  source_url="$1"
  destination="$2"
  if command -v curl >/dev/null 2>&1; then
    curl --fail --location --silent --show-error "$source_url" --output "$destination"
  elif command -v wget >/dev/null 2>&1; then
    wget --quiet "$source_url" --output-document "$destination"
  else
    fail "curl or wget is required"
  fi
}

digest() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    fail "shasum or sha256sum is required"
  fi
}

for component in $components; do
  binary="$(binary_for_component "$component")"
  archive="${binary}-v${version}-${release_target}.tar.gz"
  archive_path="${temporary_directory}/${archive}"
  checksum_path="${archive_path}.sha256"
  download "${base_url}/${archive}" "$archive_path"
  download "${base_url}/${archive}.sha256" "$checksum_path"
  expected="$(awk -v archive="$archive" 'NF == 2 && $2 == archive { print $1 }' "$checksum_path")"
  [ -n "$expected" ] || fail "invalid checksum record for $archive"
  actual="$(digest "$archive_path")"
  [ "$actual" = "$expected" ] || fail "checksum mismatch for $archive"
  extract_directory="${temporary_directory}/${binary}"
  mkdir -p "$extract_directory"
  tar -xzf "$archive_path" -C "$extract_directory"
  archive_binary="$binary"
  [ -f "${extract_directory}/${archive_binary}" ] || archive_binary="${binary}.exe"
  [ -f "${extract_directory}/${archive_binary}" ] || fail "archive does not contain $binary"
  staged="${install_dir}/.${archive_binary}.lenso-stage.$$"
  cp "${extract_directory}/${archive_binary}" "$staged"
  chmod 0755 "$staged"
  mv -f "$staged" "${install_dir}/${archive_binary}"
  echo "installed: ${install_dir}/${archive_binary}"
done

case ":${PATH}:" in
  *":${install_dir}:"*) ;;
  *) echo "add ${install_dir} to PATH before running Lenso Agent" ;;
esac
echo "Agent Home is preserved across upgrades at ${LENSO_AGENT_HOME:-${HOME}/.lenso/agent}"
