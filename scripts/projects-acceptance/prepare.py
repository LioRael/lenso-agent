"""Prepare the local Projects acceptance App from explicit source checkouts."""

import argparse
import json
import shutil
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--organization-root", required=True, type=Path)
parser.add_argument("--auth-root", required=True, type=Path)
parser.add_argument("--projects-root", required=True, type=Path)
parser.add_argument("--projects-web-root", required=True, type=Path)
parser.add_argument("--output", type=Path, default=Path(".lenso/projects-acceptance"))
args = parser.parse_args()
source = Path(__file__).resolve().parent
output = args.output.resolve()
if output.exists():
    parser.error(
        "output already exists; select a new directory to preserve previous evidence"
    )
manifest = (source / "Cargo.toml.in").read_text()
for token, root in [
    ("ORGANIZATION_ROOT", args.organization_root),
    ("AUTH_ROOT", args.auth_root),
    ("PROJECTS_ROOT", args.projects_root),
    ("PROJECTS_WEB_ROOT", args.projects_web_root),
]:
    root = root.resolve()
    if not (root / "Cargo.toml").is_file():
        parser.error(str(root) + " is not a Rust source checkout")
    # The template places placeholders inside TOML basic strings.
    manifest = manifest.replace("@" + token + "@", json.dumps(str(root))[1:-1])
output.mkdir(parents=True)
(output / "src").mkdir()
(output / "Cargo.toml").write_text(manifest)
shutil.copyfile(source / "main.rs", output / "src/main.rs")
for descriptor in source.glob("organization-*.json"):
    shutil.copyfile(descriptor, output / descriptor.name)
print(output / "Cargo.toml")

shutil.copyfile(
    args.organization_root.resolve()
    / "crates/lenso-capability-organization-directory/capability.json",
    output / "organization-directory.json",
)
