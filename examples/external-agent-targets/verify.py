"""Run real Wasm qualification outside repository Cargo configuration."""

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("--update-locks", action="store_true")
args = parser.parse_args()
source = Path(__file__).resolve().parent

with tempfile.TemporaryDirectory(prefix="lenso-target-proof-") as temporary:
    root = Path(temporary) / "consumer"
    shutil.copytree(source, root, ignore=shutil.ignore_patterns("target", ".cargo", "__pycache__"))
    for manifest in [root / "Cargo.toml", *root.glob("tests/fixtures/*/Cargo.toml")]:
        common = ["--manifest-path", str(manifest)]
        if args.update_locks:
            manifest.with_name("Cargo.lock").unlink(missing_ok=True)
            subprocess.run(["cargo", "generate-lockfile", *common], cwd=root, check=True)
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--locked", *common, "--format-version", "1"], cwd=root,
        ))
        for package in metadata["packages"]:
            if package["source"] is None:
                assert Path(package["manifest_path"]).is_relative_to(root), package
            else:
                assert package["source"] == "registry+https://github.com/rust-lang/crates.io-index", package
        if args.update_locks:
            shutil.copyfile(manifest.with_name("Cargo.lock"), source / manifest.relative_to(root).with_name("Cargo.lock"))
    subprocess.run(["cargo", "test", "--locked", "--test", "wasm_host"], cwd=root, check=True)
