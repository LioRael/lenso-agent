# Changelog

## 0.1.0 - 2026-08-31

- Ship installable `lenso-agent`, `lenso-agent-cli`, and `lenso-agent-acp`
  binaries for Apple silicon macOS and x86-64 Linux.
- Add a checksum-verifying installer with independent components, atomic binary
  replacement, upgrade reuse, preserved-state uninstall, and explicit purge.
- Add `lenso-agent-cli --version` and non-secret `doctor [--json]` diagnostics.
- Add native release CI, clean-room lifecycle acceptance, deterministic
  archives, SHA-256 indexes, and GitHub build-provenance attestations.
- Prepare the live ACP Registry submission from exact GitHub Release assets.
