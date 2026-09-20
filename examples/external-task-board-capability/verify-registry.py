"""Test the independent native consumer against locked crates.io artifacts."""

from __future__ import annotations

import json
from pathlib import Path
import shutil
import subprocess
import tempfile


def run(arguments: list[str], root: Path) -> str:
    result = subprocess.run(
        arguments, cwd=root, text=True, capture_output=True, timeout=600
    )
    if result.returncode:
        raise RuntimeError(f"{arguments}\n{result.stdout}\n{result.stderr}")
    return result.stdout


source = Path(__file__).resolve().parent
with tempfile.TemporaryDirectory(prefix="lenso-registry-consumer-") as temporary:
    root = Path(temporary).resolve() / "consumer"
    shutil.copytree(
        source, root,
        ignore=shutil.ignore_patterns("target", "dist", ".cargo", "__pycache__"),
    )
    manifest = root / "runner/Cargo.toml"
    template = (root / "runner/Cargo.toml.in").read_text()
    assert template.count("# LENSO_LOCAL_PATCHES") == 1
    manifest.write_text(template.replace("# LENSO_LOCAL_PATCHES", ""))
    common = ["--locked", "--manifest-path", str(manifest)]
    metadata = json.loads(run(["cargo", "metadata", *common, "--format-version", "1"], root))
    artifacts = []
    for package in metadata["packages"]:
        name = package["name"]
        if name == "lenso" or name.startswith("lenso-"):
            assert package["source"] == "registry+https://github.com/rust-lang/crates.io-index", package
            artifacts.append({"name": name, "version": package["version"], "source": package["source"]})
        elif package["source"] is None:
            assert Path(package["manifest_path"]).resolve().is_relative_to(root), package
    assert artifacts, "no registry framework artifacts were resolved"
    print(run(["cargo", "test", *common, "--test", "foundation"], root))
    print(json.dumps({"target": "trusted-native", "artifacts": artifacts}, indent=2))
    print("PASS: locked public registry dependencies; no sibling or Git patches. "
          "This qualifies the native Task Board consumer, not the new Agent contracts or isolation targets.")
