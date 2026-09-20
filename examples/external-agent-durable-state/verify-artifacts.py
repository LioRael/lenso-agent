"""Qualify prepared SDK archives outside the source checkout without patches.

This is release preparation, not registry publication or an isolation claim.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("--artifacts-dir", required=True, type=Path)
args = parser.parse_args()
names = ["lenso-capability-agent-durable-task", "lenso-capability-agent-extension-state"]
source = Path(__file__).resolve().parent


def run(arguments: list[str], root: Path) -> str:
    result = subprocess.run(arguments, cwd=root, text=True, capture_output=True, timeout=600)
    if result.returncode:
        raise RuntimeError(f"{arguments}\n{result.stdout}\n{result.stderr}")
    return result.stdout


with tempfile.TemporaryDirectory(prefix="lenso-prepared-sdk-consumer-") as temporary:
    root = Path(temporary).resolve()
    packages = root / "packages"
    packages.mkdir()
    receipts = []
    for name in names:
        archive = args.artifacts_dir / f"{name}-0.1.0.crate"
        receipts.append({"name": name, "version": "0.1.0", "sha256": hashlib.sha256(archive.read_bytes()).hexdigest()})
        with tarfile.open(archive) as tar:
            for member in tar.getmembers():
                assert member.isfile() or member.isdir(), member.name
                assert (packages / member.name).resolve().is_relative_to(packages), member.name
            tar.extractall(packages)
    consumer = root / "consumer"
    shutil.copytree(source, consumer, ignore=shutil.ignore_patterns("target", "dist", ".cargo", "__pycache__"))
    manifest = consumer / "runner/Cargo.toml"
    manifest.write_text((consumer / "runner/Cargo.toml.in").read_text().replace("# LENSO_LOCAL_PATCHES", ""))
    for path in [manifest, *consumer.glob("app/*/Cargo.toml")]:
        contents = path.read_text()
        for name in names:
            needle = f'{name} = "0.1.0"'
            assert needle in contents, path
            package = packages / f"{name}-0.1.0"
            contents = contents.replace(needle, f'{name} = {{ version = "=0.1.0", path = {json.dumps(str(package))} }}')
        path.write_text(contents)
    run(["cargo", "generate-lockfile", "--manifest-path", str(manifest)], root)
    metadata = json.loads(run(["cargo", "metadata", "--locked", "--manifest-path", str(manifest), "--format-version", "1"], root))
    for package in metadata["packages"]:
        if package["name"].startswith("lenso") and package["name"] not in names:
            assert package["source"] == "registry+https://github.com/rust-lang/crates.io-index", package
        elif package["source"] is None:
            assert Path(package["manifest_path"]).resolve().is_relative_to(root), package
    print(run(["cargo", "test", "--locked", "--manifest-path", str(manifest), "--test", "restart"], root))
    print(json.dumps({"prepared_archives": receipts, "lock_sha256": hashlib.sha256((manifest.parent / "Cargo.lock").read_bytes()).hexdigest()}, indent=2))
    print("PASS: unpacked versioned SDK archives with public registry dependencies; no sibling or Cargo patches. Not registry publication.")
