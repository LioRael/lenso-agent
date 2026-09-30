"""Build the packaged Tool Provider against only released registry dependencies."""

from __future__ import annotations

import pathlib
import subprocess
import tarfile
import tempfile
import tomllib


ROOT = pathlib.Path(__file__).resolve().parents[1]
CODEC_VERSION = "0.4.4"
manifest = tomllib.loads(
    (ROOT / "crates/lenso-capability-agent-tool-provider/Cargo.toml").read_text()
)
version = manifest["package"]["version"]
archive = ROOT / f"target/package/lenso-capability-agent-tool-provider-{version}.crate"
package_name = f"lenso-capability-agent-tool-provider-{version}"


def run(*args: str, cwd: pathlib.Path) -> None:
    subprocess.run(["cargo", *args], cwd=cwd, check=True)


with tempfile.TemporaryDirectory(prefix="lenso-tool-provider-registry-") as temporary:
    directory = pathlib.Path(temporary)
    with tarfile.open(archive, "r:gz") as package:
        members = package.getmembers()
        if any(
            pathlib.PurePosixPath(member.name).parts[0] != package_name
            for member in members
        ):
            raise SystemExit("Tool Provider package contains an unexpected root")
        package.extractall(directory, filter="data")

    packaged = directory / package_name
    (packaged / "Cargo.lock").unlink(missing_ok=True)
    normalized = tomllib.loads((packaged / "Cargo.toml").read_text())
    codec_requirement = normalized["dependencies"]["lenso-runtime-codec"]["version"]
    if codec_requirement != CODEC_VERSION:
        raise SystemExit(f"unexpected registry Codec pin: {codec_requirement}")

    run("generate-lockfile", cwd=packaged)
    run("update", "-p", "lenso-runtime-codec", "--precise", CODEC_VERSION, cwd=packaged)
    run("check", "--locked", cwd=packaged)
    lock = tomllib.loads((packaged / "Cargo.lock").read_text())
    packages = lock["package"]
    codec = [entry for entry in packages if entry["name"] == "lenso-runtime-codec"]
    if len(codec) != 1 or codec[0]["version"] != CODEC_VERSION:
        raise SystemExit(f"registry Codec {CODEC_VERSION} was not selected exactly")
    roots = [
        entry for entry in packages
        if (entry["name"], entry["version"]) == (manifest["package"]["name"], version)
    ]
    if len(roots) != 1 or roots[0].get("source") is not None:
        raise SystemExit("package-only verification has no unique local root")
    if any(
        entry is not roots[0] and not entry.get("source", "").startswith("registry+")
        for entry in packages
    ):
        raise SystemExit("package-only verification resolved a non-registry dependency")
    print(f"PASS: {package_name} builds with registry-only Codec {CODEC_VERSION}")
