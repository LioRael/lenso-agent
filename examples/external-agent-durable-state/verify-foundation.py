"""Run V05/V06 and native V09 from a copied external consumer source tree."""

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
    "lenso-capability-agent-durable-task": agent / "crates/lenso-capability-agent-durable-task",
    "lenso-capability-agent-extension-state": agent / "crates/lenso-capability-agent-extension-state",
}
for name, path in paths.items():
    assert (path / "Cargo.toml").is_file(), f"missing {name}: {path}"


with tempfile.TemporaryDirectory(prefix="lenso-agent-external-durable-state-") as temporary:
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
    (root / "runner/Cargo.toml").write_text(template.replace("# LENSO_LOCAL_PATCHES", patch))
    run(
        ["cargo", "test", "--manifest-path", root / "runner/Cargo.toml", "--test", "restart"],
        cwd=root,
    )
    print(
        "PASS: a copied external Plugin persisted owner-scoped extension facts and "
        "a serializable approval task across real process restarts, duplicate signals, "
        "safe unknown presentation, uncertain-effect recovery blocking, active-stream "
        "removal, stale-handle rejection, and version-blocked recovery."
    )
