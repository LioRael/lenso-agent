"""Verify the exact Foundation SDK release from a clean registry consumer."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("--version", default="0.1.0")
parser.add_argument("--evidence-dir", type=Path)
args = parser.parse_args()
names = ["lenso-capability-agent-" + suffix for suffix in (
    "model", "turn-processing", "extension-state", "interaction",
    "dynamic-authority", "durable-task",
)]
source = Path(__file__).resolve().parents[1] / "examples/external-agent-durable-state"


def run(command: list[str], root: Path) -> str:
    result = subprocess.run(command, cwd=root, text=True, capture_output=True, timeout=900)
    if result.returncode:
        raise RuntimeError(f"{command}\n{result.stdout}\n{result.stderr}")
    return result.stdout


with tempfile.TemporaryDirectory(prefix="lenso-foundation-registry-") as temporary:
    root = Path(temporary).resolve() / "consumer"
    shutil.copytree(source, root, ignore=shutil.ignore_patterns("target", ".cargo", "__pycache__", "Cargo.lock"))
    manifest = root / "runner/Cargo.toml"
    manifest.write_text((root / "runner/Cargo.toml.in").read_text().replace("# LENSO_LOCAL_PATCHES", ""))
    for path in [manifest, *root.glob("app/*/Cargo.toml")]:
        contents = path.read_text()
        for name in names:
            needle = f'{name} = "0.1.0"'
            if needle in contents:
                contents = contents.replace(needle, f'{name} = {json.dumps("=" + args.version)}')
            elif path == manifest:
                contents = contents.replace("[dependencies]\n", f'[dependencies]\n{name} = {json.dumps("=" + args.version)}\n', 1)
        path.write_text(contents)
    common = ["--manifest-path", str(manifest)]
    run(["cargo", "generate-lockfile", *common], root)
    metadata = json.loads(run(["cargo", "metadata", "--locked", *common, "--format-version", "1"], root))
    packages = []
    seen = set()
    for package in metadata["packages"]:
        if package["name"] == "lenso" or package["name"].startswith("lenso-"):
            assert package["source"] == "registry+https://github.com/rust-lang/crates.io-index", package
            packages.append({"name": package["name"], "version": package["version"], "source": package["source"]})
        elif package["source"] is None:
            assert Path(package["manifest_path"]).resolve().is_relative_to(root), package
        if package["name"] in names:
            assert package["version"] == args.version, package
            seen.add(package["name"])
    assert seen == set(names), seen
    output = run(["cargo", "test", "--locked", *common, "--test", "restart"], root)
    report = {"version": args.version, "packages": packages, "result": "passed", "target": "trusted-native"}
    if args.evidence_dir:
        args.evidence_dir.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(manifest.parent / "Cargo.lock", args.evidence_dir / "Cargo.lock")
        (args.evidence_dir / "registry-consumer.json").write_text(json.dumps(report, indent=2) + "\n")
        (args.evidence_dir / "registry-consumer-tests.txt").write_text(output)
    print(output)
    print(json.dumps(report, indent=2))
    print("PASS: all six exact SDK versions consumed from crates.io without patches; durable lifecycle tests passed.")
