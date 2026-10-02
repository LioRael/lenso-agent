#!/usr/bin/env python3
"""Validate the existing SDK cohort and prepared Wasm locks without building."""
import importlib.util
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("tool_release", ROOT / "scripts/verify-tool-sdk-release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
def check_locks(root, manifests):
    for manifest in manifests:
        lock = manifest.with_name("Cargo.lock")
        if not lock.is_file():
            raise RuntimeError(f"missing prepared fixture lock: {lock.relative_to(root)}")
        original = lock.read_bytes()
        print(f"Locked metadata: {manifest.relative_to(root)}", flush=True)
        subprocess.run(["cargo", "metadata", "--locked", "--format-version", "1",
                        "--manifest-path", str(manifest)], cwd=root, check=True, stdout=subprocess.DEVNULL)
        if lock.read_bytes() != original:
            raise RuntimeError(f"locked metadata mutated {lock}")


if __name__ == "__main__":
    print(f"Tool SDK cohort: {release.cohort_versions()}", flush=True)
    root = ROOT / "examples/external-agent-targets"
    paths = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z",
         "examples/external-agent-targets/tests/fixtures/*/Cargo.toml"], cwd=ROOT
    ).decode().split("\0")
    check_locks(ROOT, [ROOT / "Cargo.toml", root / "Cargo.toml", *[ROOT / path for path in paths if path]])
