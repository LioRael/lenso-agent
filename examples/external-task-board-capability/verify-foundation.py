"""Verify the external Capability fixture through a copied source-local App."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile


def discover_framework_root(source: Path) -> Path:
    for candidate in (source, *source.parents):
        if all(
            (candidate / manifest).is_file()
            for manifest in (
                "lenso/crates/lenso-app-plan/Cargo.toml",
                "lenso-protocols/crates/lenso-contract-runtime/Cargo.toml",
                "lenso-runtime-rust/crates/lenso/Cargo.toml",
            )
        ):
            return candidate
    raise AssertionError(
        "could not discover the framework root; pass --framework-root explicitly"
    )


parser = argparse.ArgumentParser()
parser.add_argument("--engine-host", required=True, type=Path)
parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parent)
parser.add_argument("--framework-root", type=Path)
args = parser.parse_args()

engine = args.engine_host.resolve()
source = args.source.resolve()
framework = (
    args.framework_root.resolve()
    if args.framework_root is not None
    else discover_framework_root(source)
)
assert engine.is_file(), engine
assert (source / "runner/Cargo.toml.in").is_file(), source


def lenso_paths(root: Path) -> dict[str, Path]:
    paths = {
        "lenso": root / "lenso-runtime-rust/crates/lenso",
        "lenso-app-plan": root / "lenso/crates/lenso-app-plan",
        "lenso-contract-runtime": root / "lenso-protocols/crates/lenso-contract-runtime",
        "lenso-guest-sdk": root / "lenso-runtime-rust/crates/lenso-guest-sdk",
        "lenso-kernel": root / "lenso/crates/lenso-kernel",
        "lenso-native-adapter": root / "lenso-runtime-rust/crates/lenso-native-adapter",
        "lenso-plugin-authoring": root / "lenso-protocols/crates/lenso-plugin-authoring",
        "lenso-runner": root / "lenso-runtime-rust/crates/lenso-runner",
        "lenso-runtime-codec": root / "lenso-runtime-rust/crates/lenso-runtime-codec",
    }
    for name, path in paths.items():
        assert (path / "Cargo.toml").is_file(), f"missing {name}: {path}"
    return paths


paths = lenso_paths(framework)


def run(arguments: list[Path | str], *, cwd: Path) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        [str(value) for value in arguments],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=300,
    )
    assert completed.returncode == 0, (
        f"command failed: {' '.join(str(value) for value in arguments)}\n"
        f"stdout:\n{completed.stdout}\n"
        f"stderr:\n{completed.stderr}"
    )
    return completed


def toml_path(path: Path) -> str:
    return json.dumps(str(path))


def prepare_local_source(root: Path) -> None:
    for manifest in (root / "app").glob("*/Cargo.toml"):
        contents = manifest.read_text()
        assert 'lenso = "0.5.21"' in contents, manifest
        manifest.write_text(
            contents.replace(
                'lenso = "0.5.21"',
                f"lenso = {{ path = {toml_path(paths['lenso'])} }}",
            )
        )

    patch = "\n".join(
        ["[patch.crates-io]"]
        + [f"{name} = {{ path = {toml_path(path)} }}" for name, path in paths.items()]
    )
    template = (root / "runner/Cargo.toml.in").read_text()
    assert template.count("# LENSO_LOCAL_PATCHES") == 1
    (root / "runner/Cargo.toml").write_text(
        template.replace("# LENSO_LOCAL_PATCHES", patch)
    )


with tempfile.TemporaryDirectory(prefix="lenso-agent-external-task-board-") as temporary:
    root = Path(temporary) / "consumer"
    shutil.copytree(
        source,
        root,
        ignore=shutil.ignore_patterns("target", "dist", "__pycache__", "*.pyc"),
    )
    prepare_local_source(root)
    distribution = root / "dist"

    run([engine, "app", "assemble", "--root", root, "--out", distribution], cwd=root)
    run([engine, "app", "start", "--from", distribution, "--check"], cwd=root)
    run(
        ["cargo", "test", "--manifest-path", root / "runner/Cargo.toml", "--test", "foundation"],
        cwd=root,
    )

    sources = json.loads((distribution / "local-sources.json").read_text())
    source_ids = {item["plugin_id"] for item in sources["sources"]}
    assert source_ids == {
        "example.external-task-board-caller",
        "example.external-task-board-provider",
        "example.external-task-board-report",
    }, sources
    generated_manifest = (distribution / ".lenso/generated-host/Cargo.toml").read_text()
    assert "[patch.crates-io.lenso]" in generated_manifest, generated_manifest
    assert str(paths["lenso"]) in generated_manifest, generated_manifest
    print(
        "PASS: an independent external Capability source assembled three local "
        "Plugins, resolved its public plan, preserved cancellation and domain "
        "errors, and stopped each lifecycle cleanly."
    )
