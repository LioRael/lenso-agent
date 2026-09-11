# Package-manager distribution

Agent owns the native executables and versioned GitHub release archives. Two
consumers package those exact bytes for end users:

- [Lenso Homebrew tap](https://github.com/LioRael/homebrew-tap): terminal UI,
  management CLI, and ACP, versioned with Agent.
- [Console npm distribution](https://github.com/LioRael/lenso-console/blob/main/docs/agent-npm-distribution.md):
  `@lenso/agent` and its exact platform dependencies, versioned with Console.
  Includes the native terminal entrypoints and the complete browser UI.

Both consumers pin archive SHA-256 digests from a published Agent release.
They do not rebuild Agent or download executables at application startup.
Publishing a native release does not by itself publish either downstream package.

## Release sequence

1. Publish and verify the native Agent release through this repository's Release
   workflow. Keep its archives and checksums immutable.
2. Dispatch the tap's **Update Agent** workflow with that exact version. It
   generates a Formula update PR and dispatches macOS/Linux install checks:

   ```sh
   gh workflow run update.yml --repo LioRael/homebrew-tap -f version=0.1.10
   ```

   Review and merge the update after both installation checks pass.
3. In Console, update `scripts/distribution/agent-release.json` with the version
   and all ten platform/component digests. Include a changeset, then complete
   the Console version PR. Run **Agent npm distribution** with `publish=false`
   before its authorized `publish=true` run on `main`.
4. Verify the registry's launcher and both platform versions, install through
   each published package manager, and check `tui --version` or the native
   `lenso-agent --version` against the intended Agent version.

The npm workflow uses its existing OIDC Trusted Publishers; Homebrew updates
use the tap repository's own GitHub token. No shared cross-repository release
token is required. A successful build or merged PR is not publication proof.
