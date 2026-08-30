# ADR-0091: Distribute installable Lenso Agent releases

Status: Accepted

## Context

Lenso Agent has stable product entrypoints and a versioned ACP packaging
primitive, but its public workflow still requires a Rust checkout. Cargo
packages intentionally remain private because each executable links a complete
Host Catalog and many internal Plugin crates. Making every implementation crate
public merely to support `cargo install` would turn private composition seams
into accidental SemVer commitments.

The process-owning surfaces remain independent under ADR-0042. A product
installer may select more than one independent release archive without merging
their Host Catalogs or creating a second App definition.

## Decision

- GitHub Releases are the first binary distribution authority. Cargo packages
  remain `publish = false`.
- Each archive contains exactly one of `lenso-agent`, `lenso-agent-cli`, or
  `lenso-agent-acp` and is named by exact version and target. The initial target
  matrix is Apple silicon macOS and x86-64 Linux.
- The default installer selects the interactive and management archives. ACP
  remains an explicit optional component. Web and Channel releases stay out of
  the first local coding product slice.
- Installation verifies a sibling SHA-256 record before atomically replacing a
  binary. Release archives also receive GitHub build-provenance attestations.
- Ordinary upgrade and uninstallation never delete Agent Home. Durable state is
  removed only by a separately named purge option with broad-path guards.
- `lenso-agent-cli doctor [--json]` exposes non-secret version, installation,
  authentication, Agent Home, and dependency checks without moving Plugin
  resolution or platform discovery into Kernel.
- Zed distribution continues through the ACP Registry after the exact GitHub
  Release URLs and checksums are live.

## Consequences

- A user can install and run the terminal product without Rust or a source
  checkout.
- Independent Host Catalogs and removable surface Plugins remain unchanged.
- Adding another operating system requires its own native build and clean-room
  acceptance evidence rather than only adding a target name to a manifest.
- Binary rollback does not imply durable-state downgrade compatibility; each
  Host release must continue to validate or fail closed on stored schemas.
- The initial macOS prerelease relies on archive checksums and GitHub
  build-provenance attestations; it does not claim platform identity.

## Proof

Pull requests build and test the native macOS and Linux entrypoints, reproduce
archives, reject a tampered download without replacing the installed binary,
install all three public entrypoints into an empty prefix, run version and
doctor checks, reinstall as an upgrade, uninstall while preserving Agent Home,
and render the ACP Registry entry from the final checksums. Published workflows
add provenance attestations and verify every release asset before creating the
GitHub prerelease.
