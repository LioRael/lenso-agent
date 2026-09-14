# Release size

Measure the terminal installation as well as each executable. A terminal-only
installation (`--component agent`) contains `lenso-agent` and `lenso-agent-cli`;
adding ACP also installs `lenso-agent-acp`. The default installer additionally
includes both Web entrypoints. Archive download size and uncompressed installed
bytes are different measurements.

```sh
cargo build --locked --release \
  -p lenso-agent-tui -p lenso-agent-cli -p lenso-agent-acp
python3 scripts/report-release-size.py target/release --output sizes.json
```

The release workflow records the same report for each platform in
`terminal-size-<target>.json`. Release checksums include these reports. The
report counts logical file bytes, not filesystem allocation or runtime memory.
When both Web binaries are present, it also reports the default installation
and the total for all five release entrypoints. Missing Web binaries are not
counted as zero-sized files.

## Compilation policy

Release builds use Thin LTO, one codegen unit, and symbol stripping. These apply
to all release entrypoints, including the Web surfaces. Panic unwinding, Plugin
registrations, execution adapters, and the separate surface Host Catalogs remain
unchanged. Link-time optimization can increase build time and peak memory;
symbol stripping reduces names available for postmortem debugging. A diagnostic
build can override stripping with `CARGO_PROFILE_RELEASE_STRIP=none`.

Do not switch to `panic = "abort"` as a size-only tweak: the Runner's local task
and replicated-lane paths use `catch_unwind` to report task/lane failure.
Aborting changes that behavior to process termination and bypasses unwinding
cleanup, including the terminal restoration path described in ADR-0020.

## Framework follow-up

The three executables each contain their shared Host and default Plugin code.
CLI and ACP do not depend on Ratatui or Crossterm. ADR-0042 and ADR-0091 require
independent surface distributions and Catalogs; replacing them with one multicall
binary or a shared dynamic runtime needs a separate compatibility and packaging
decision. Sharing source crates does not share installed machine code.

The dependency audit found these narrower opportunities:

- Align the authoring library's JSON Schema version with the Host's validator.
  `lenso-cli` 0.5.2 uses 0.47 while the Host uses 0.51. Both have runtime library
  callers, but the release experiment below found no meaningful executable saving.
- Align ZIP backends in authoring tools. In consumed `lenso-cli` 0.5.2, ZIP and
  Ureq are binary-only callers despite unconditional dependencies. Removing them
  from the library build closure does not establish an executable byte saving.
- Narrow Host Catalog assembly for products that do not support remote and dylib
  execution. Agent advertises native, QuickJS, process, Bun, and Wasm execution
  classes. Preserve the existing framework API even for consumers that disable
  default features, and reject unavailable execution classes explicitly.
- Investigate the Bun adapter's TLS-enabled JSON-RPC client for its loopback HTTP
  transport. Check its transport tests before removing the TLS dependency.
- Consolidate TLS providers only with coordinated provider selection. Reqwest's
  `rustls` feature enables AWS-LC while the direct Codex WebSocket and Bun paths
  enable Ring. Changing one caller alone does not remove the other backend.

Wasmtime/Cranelift and Lua implement supported runtime behavior. Removing them
would be a capability change, not a compilation-only optimization. Dependency
tree entries alone do not measure retained executable bytes; compare binaries
built from the same source and toolchain after each change.

## Local measurement: 2026-09-14

The terminal comparison uses source commit `5cf171ef529553f8ef8a8767a224a0937c044073`
(`0.1.12`), macOS ARM64, Cargo `1.99.0-nightly (59800466c 2026-07-07)`, and
the same locked dependency versions. It does not include the separate authoring
dependency update. MB below means 1,000,000 bytes.

| Executable | Original release | Strip-only copy | Release configuration above |
| --- | ---: | ---: | ---: |
| `lenso-agent` | 75.78 MB | 58.39 MB | 47.33 MB |
| `lenso-agent-cli` | 74.48 MB | 57.37 MB | 46.60 MB |
| `lenso-agent-acp` | 75.10 MB | 57.74 MB | 47.06 MB |
| **Terminal plus ACP** | **225.35 MB** | **173.51 MB** | **141.00 MB** |

The optimized total is 140,997,488 bytes, down 84,354,144 bytes (37.4%) from
225,351,632 bytes. The strip-only experiment used `strip -x` and a new ad-hoc
signature on copies. The final column measures Cargo's complete release
configuration, not an isolated LTO effect.

The observed builds took 5m38s and 11m41s respectively. These are local build
observations with existing caches, not a controlled clean-build benchmark.
Release version checks, TUI startup, saturated Skill/file completion, clean
exit, sibling command dispatch, and `doctor --json` passed. The baseline TUI
smoke also passed. Linux and the Web release entrypoints were not built in this
local comparison; the release workflow retains their existing checks.

The separate `lenso-cli` worktree (base `9af362d`, version `0.5.3`) aligns
`jsonschema` to 0.51 and ZIP to 8.6 with the `deflate-flate2-zlib-rs` backend.
Its default-feature and library-only builds, policy tests, archive tests, and
fixture-gated archive/HTTPS checks passed without source or fixture changes.

A temporary Cargo patch integrated that library into Agent for validation. The
resolved graph contained only `jsonschema 0.51.0` and `zip 8.6.0`. Debug-profile
tests passed for both surface Catalogs, the headless tool turn followed by
durable session resume, and both ACP stdio lifecycle/cancellation cases (five
tests in total). The original Agent lockfile was restored afterward. This
dependency change still needs its framework release and a permanent Agent
dependency update; it is not included in the release-size numbers above.

## Second-round experiments: 2026-09-14

The authoring dependency pilot produced a 46,604,016-byte CLI, compared with
46,603,120 bytes in the first-round optimized build. This is effectively
unchanged. The pilot built CLI alone and updated the authoring/bundle cohort,
whereas the first-round build selected all three terminal packages together;
these numbers are a screening result, not an isolated attribution of 896 bytes
to a dependency. Dependency deduplication remains useful maintenance but is not
credited as an executable-size reduction.

A second experiment held the runtime source cohort constant (runtime base
`3d26ff4`, facade `0.5.19`, control plane `0.4.18`) and compared CLI-only builds
with the same release profile and toolchain. A feature-gated prototype omitted
remote/dylib adapters and produced 46,118,288 bytes, versus 46,587,120 bytes for
the full adapter build: 468,832 bytes smaller (about 1.0%). Version/doctor checks,
the durable headless turn, both distribution Catalog tests, and ACP stdio tests
passed. This prototype is **not the accepted compatibility design**: adding
feature gates to existing public methods changes availability for direct
control-plane consumers using `default-features = false`. Preserving the usual
default build alone is insufficient.

The compatible implementation adds `CoreExecutionCatalogFactory`, reexported
through `lenso::host`, while retaining `MultiExecutionCatalogFactory` and all
existing feature configurations. Both factories share core assembly and
availability validation. The full factory still configures remote clients and
requires a trust verifier for dylib selection. The core factory lets LTO discard
unused adapter implementations without removing dependencies from the build
graph. After freezing the final framework source, the full-factory CLI measured
46,603,744 bytes and the core-factory CLI measured 46,117,488 bytes: a reduction
of 486,256 bytes (1.04%). This same-cohort comparison, rather than a comparison
against the first-round dependency cohort, is the accepted second-round result.

Control-plane default and no-default-feature tests pass (16 each), as do facade
Host lifecycle tests, formatting, and scoped clippy with `--no-deps -D warnings`.
Clippy including dependencies still reports the pre-existing deprecated
`AtomicU64::fetch_update` use in `lenso-process-adapter`; this unrelated warning
was not changed as part of the size work.

Agent adoption was validated with a temporary framework patch and a three-site
replacement of `MultiExecutionCatalogFactory` with `CoreExecutionCatalogFactory`
in the Host's `generation.rs`. Agent's original source and lockfile are restored
after the experiment. Permanent adoption must follow a framework release that
exports the new factory; no machine-local Cargo patch belongs in the release.
Release `--version` and `doctor --json` passed for both CLI variants. With the
core factory selected, the durable headless tool turn/resume test, both surface
Catalog tests, and both ACP stdio lifecycle/cancellation tests passed (five
Agent tests). This round measures CLI only and does not extrapolate savings to
the complete installation.
