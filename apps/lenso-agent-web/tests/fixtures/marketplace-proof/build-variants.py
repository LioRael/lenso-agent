#!/usr/bin/env python3
"""Build real install/update/startup-failure archives without altering the source fixture.

Usage: LENSO=/path/to/lenso CARGO=cargo python3 build-variants.py OUTPUT_DIR
Run inside the repository so local Cargo wrappers can identify the owner.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

source = Path(__file__).resolve().parent
output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
for version, prefix, name in [
    ("0.1.0", "", "marketplace_proof"),
    ("0.2.0", "v2: ", "marketplace_proof"),
    ("0.3.0", "", "invalid.tool.name"),
]:
    with tempfile.TemporaryDirectory(prefix=".proof-", dir=source) as directory:
        project = Path(directory)
        shutil.copytree(source / "src", project / "src")
        manifest = (source / "Cargo.toml").read_text().replace('version = "0.1.0"', f'version = "{version}"')
        (project / "Cargo.toml").write_text(manifest)
        code = (project / "src/lib.rs").read_text().replace('name = "marketplace_proof"', f'name = "{name}"')
        if prefix:
            code = code.replace('content: arguments.text,', f'content: format!("{prefix}{{}}", arguments.text),')
        (project / "src/lib.rs").write_text(code)
        # Packing synchronizes the version offline; resolve a fresh fixture first.
        subprocess.run([os.environ.get("CARGO", "cargo"), "generate-lockfile", "--manifest-path", str(project / "Cargo.toml")], check=True)
        subprocess.run([os.environ.get("LENSO", "lenso"), "plugin", "pack", "--repo-root", str(project), "--output", str(output / f"proof-{version}.lenso-plugin"), "--json"], check=True)
