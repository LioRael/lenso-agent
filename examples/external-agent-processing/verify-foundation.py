"""Run V04 from a copied external consumer source tree."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile


parser = argparse.ArgumentParser()
parser.add_argument("--framework-root", required=True, type=Path)
parser.add_argument("--agent-root", required=True, type=Path)
parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parent)
args = parser.parse_args()

framework = args.framework_root.resolve()
agent = args.agent_root.resolve()
source = args.source.resolve()
assert (framework / "lenso").is_dir(), framework
assert (framework / "lenso-protocols").is_dir(), framework
assert (framework / "lenso-runtime-rust").is_dir(), framework
assert (agent / "Cargo.toml").is_file(), agent


def toml_path(path: Path) -> str:
    return json.dumps(str(path))


def run(arguments: list[str | Path], *, cwd: Path) -> None:
    completed = subprocess.run(
        [str(value) for value in arguments],
        cwd=cwd,
        text=True,
        capture_output=True,
        timeout=300,
    )
    assert completed.returncode == 0, (
        f"command failed: {' '.join(str(value) for value in arguments)}\n"
        f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
    )


paths = {
    "lenso": framework / "lenso-runtime-rust/crates/lenso",
    "lenso-app-plan": framework / "lenso/crates/lenso-app-plan",
    "lenso-contract-runtime": framework / "lenso-protocols/crates/lenso-contract-runtime",
    "lenso-guest-sdk": framework / "lenso-runtime-rust/crates/lenso-guest-sdk",
    "lenso-kernel": framework / "lenso/crates/lenso-kernel",
    "lenso-native-adapter": framework / "lenso-runtime-rust/crates/lenso-native-adapter",
    "lenso-plugin-authoring": framework / "lenso-protocols/crates/lenso-plugin-authoring",
    "lenso-runner": framework / "lenso-runtime-rust/crates/lenso-runner",
    "lenso-runtime-codec": framework / "lenso-runtime-rust/crates/lenso-runtime-codec",
}
for name in (
    "lenso-capability-agent",
    "lenso-capability-agent-model",
    "lenso-capability-agent-tool-hook",
    "lenso-capability-agent-tool-provider",
    "lenso-capability-agent-tools",
    "lenso-capability-agent-turn-processing",
    "lenso-agent-tools-plugin",
):
    paths[name] = agent / "crates" / name
for name, path in paths.items():
    assert (path / "Cargo.toml").is_file(), f"missing {name}: {path}"


with tempfile.TemporaryDirectory(prefix="lenso-agent-external-processing-") as temporary:
    root = Path(temporary) / "consumer"
    shutil.copytree(
        source,
        root,
        ignore=shutil.ignore_patterns("target", "dist", "__pycache__", "*.pyc"),
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
    run(
        ["cargo", "test", "--manifest-path", root / "runner/Cargo.toml", "--test", "processing"],
        cwd=root,
    )
    print(
        "PASS: a copied external consumer composed a third-party Agent Loop, "
        "ordered request/result processors, final Tool approval, schema revalidation, "
        "cancellation, and provenance without the default Loop."
    )
