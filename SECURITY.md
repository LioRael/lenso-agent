# Security policy

## Supported version

The newest Lenso Agent prerelease is the only version receiving security fixes
before the first stable release. Release notes identify the exact supported
targets and trust properties.

## Report a vulnerability

Use GitHub's private vulnerability reporting flow for
[`LioRael/lenso-agent`](https://github.com/LioRael/lenso-agent/security/advisories/new).
Do not open a public issue for credentials, privilege-boundary failures, or an
unreleased vulnerability.

Include the affected version and platform, the Plugin/Profile involved, the
expected authority boundary, and the smallest non-secret reproduction. Never
attach live tokens, private keys, or a populated Agent Home.

Native Rust, Bun, and installed package code are trusted code. The native
Process Plugin is not a security sandbox. The `code-sandbox` Profile provides
the narrower macOS Seatbelt or Linux bubblewrap boundary documented in the
README; it is not a confidentiality boundary or virtual machine.
