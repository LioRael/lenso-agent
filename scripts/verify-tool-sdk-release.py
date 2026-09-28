"""Verify source cohort archives and the selected published Tool package set."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import time
import tomllib
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


ROOT = Path(__file__).resolve().parents[1]
PROVIDER = "lenso-capability-agent-tool-provider"
MACROS = "lenso-agent-tool-sdk-macros"
SDK = "lenso-agent-tool-sdk"
CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
MAX_ARCHIVE_BYTES = 16 * 1024 * 1024


def cohort_versions() -> dict[str, str]:
    manifests = {
        name: tomllib.loads((ROOT / "crates" / name / "Cargo.toml").read_text())
        for name in (PROVIDER, MACROS, SDK)
    }
    versions = {name: manifest["package"]["version"] for name, manifest in manifests.items()}
    if any(manifest["package"].get("publish") is not True for manifest in manifests.values()):
        raise RuntimeError("all three cohort crates must be publishable")
    dependencies = manifests[SDK]["dependencies"]
    if (
        versions[SDK] != versions[MACROS]
        or dependencies[MACROS]["version"] != versions[MACROS]
        or dependencies[PROVIDER]["version"] != versions[PROVIDER]
    ):
        raise RuntimeError("Tool SDK cohort versions do not match")
    return versions


def selected_revision() -> str:
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if os.environ.get("GITHUB_SHA", revision) != revision:
        raise RuntimeError("checked-out commit differs from GitHub event SHA")
    return revision


def verify_archive(
    name: str, version: str, contents: bytes, versions: dict[str, str], revision: str | None = None
) -> None:
    package_root = f"{name}-{version}"
    with tarfile.open(fileobj=io.BytesIO(contents), mode="r:gz") as package:
        members = package.getmembers()
        if not members or any(
            not Path(member.name).parts or Path(member.name).parts[0] != package_root
            for member in members
        ):
            raise RuntimeError(f"{package_root}: unexpected package root")
        normalized = package.extractfile(f"{package_root}/Cargo.toml")
        if normalized is None:
            raise RuntimeError(f"{package_root}: missing normalized manifest")
        manifest = tomllib.loads(normalized.read().decode())
        if revision is not None:
            provenance = package.extractfile(f"{package_root}/.cargo_vcs_info.json")
            if provenance is None:
                raise RuntimeError(f"{package_root}: missing Cargo VCS provenance")
            vcs = json.loads(provenance.read())
            if vcs.get("git", {}).get("sha1") != revision or vcs.get("path_in_vcs") != f"crates/{name}":
                raise RuntimeError(f"{package_root}: published archive is not from the selected commit")
    if (manifest["package"]["name"], manifest["package"]["version"]) != (name, version):
        raise RuntimeError(f"{package_root}: package identity differs from source manifest")
    if name == SDK:
        for dependency in (PROVIDER, MACROS):
            specification = manifest["dependencies"][dependency]
            if specification.get("version") != versions[dependency] or any(
                key in specification for key in ("path", "git", "registry")
            ):
                raise RuntimeError(f"{package_root}: {dependency} is not a crates.io cohort dependency")


def verify_local_archives(versions: dict[str, str], revision: str) -> None:
    for name, version in versions.items():
        archive = ROOT / "target/package" / f"{name}-{version}.crate"
        verify_archive(name, version, archive.read_bytes(), versions, revision)


def verify_download(name: str, version: str, versions: dict[str, str], revision: str | None) -> str:
    url = f"https://crates.io/api/v1/crates/{name}/{version}/download"
    request = Request(url, headers={"User-Agent": "lenso-tool-sdk-release-verifier/1"})
    for attempt in range(12):
        try:
            with urlopen(request, timeout=15) as response:
                published = response.read(MAX_ARCHIVE_BYTES + 1)
        except HTTPError as error:
            if error.code not in (404, 429, 500, 502, 503, 504) or attempt == 11:
                raise RuntimeError(f"{name} {version}: registry readback failed") from error
        except URLError as error:
            if attempt == 11:
                raise RuntimeError(f"{name} {version}: registry readback failed") from error
        else:
            if len(published) > MAX_ARCHIVE_BYTES:
                raise RuntimeError(f"{name} {version}: published archive is unexpectedly large")
            verify_archive(name, version, published, versions, revision)
            checksum = hashlib.sha256(published).hexdigest()
            suffix = " with the selected Git commit" if revision is not None else ""
            print(f"PASS: {name} {version} archive is downloadable{suffix}")
            return checksum
        time.sleep(5)
    raise RuntimeError(f"{name} {version}: registry readback timed out")


def verify_registry_checksums(
    packages: list[dict], versions: dict[str, str], checksums: dict[str, str]
) -> None:
    for name, version in versions.items():
        matches = [package for package in packages if package["name"] == name]
        if (
            len(matches) != 1
            or matches[0]["version"] != version
            or matches[0].get("source") != CRATES_IO_SOURCE
            or matches[0].get("checksum") != checksums[name]
        ):
            raise RuntimeError(f"{name} {version}: downloaded bytes differ from crates.io index")


def verify_consumer(versions: dict[str, str], checksums: dict[str, str]) -> None:
    selected_versions = {name: versions[name] for name in checksums}
    provider_only = set(selected_versions) == {PROVIDER}
    if not provider_only and set(selected_versions) != {PROVIDER, MACROS, SDK}:
        raise RuntimeError("unsupported Tool release set")
    consumer_name = (
        "lenso-tool-provider-registry-smoke" if provider_only
        else "lenso-tool-sdk-registry-smoke"
    )
    with tempfile.TemporaryDirectory(prefix=f"{consumer_name}-") as temporary:
        root = Path(temporary)
        (root / "src").mkdir()
        dependencies = "\n".join(
            f'{name} = "={version}"' for name, version in selected_versions.items()
        )
        (root / "Cargo.toml").write_text(
            f'[package]\nname = "{consumer_name}"\nversion = "0.0.0"\n'
            'edition = "2024"\n\n[dependencies]\n' + dependencies + "\n"
        )
        if provider_only:
            surface = "lenso_capability_agent_tool_provider"
        else:
            surface = "lenso_agent_tool_sdk::prelude::tool_provider_contract"
        (root / "src/main.rs").write_text(
            "fn main() {\n"
            f"    let _ = std::any::type_name::<{surface}::ToolProviderJsonCodec>();\n"
            f"    let _ = std::any::type_name::<{surface}::ToolProviderClient>();\n"
            "}\n"
        )
        environment = {key: value for key, value in os.environ.items() if not key.startswith("CARGO_")}
        environment["CARGO_HOME"] = str(root / "cargo-home")
        environment["CARGO_TARGET_DIR"] = str(root / "target")

        def cargo(*arguments: str, capture: bool = False) -> str:
            result = subprocess.run(
                ["cargo", *arguments], cwd=root, env=environment, check=True,
                text=True, capture_output=capture, timeout=900,
            )
            return result.stdout if capture else ""

        cargo("generate-lockfile")
        lock = tomllib.loads((root / "Cargo.lock").read_text())
        verify_registry_checksums(lock["package"], selected_versions, checksums)
        metadata = json.loads(cargo("metadata", "--locked", "--format-version", "1", capture=True))
        selected: dict[str, set[str]] = {name: set() for name in selected_versions}
        for package in metadata["packages"]:
            if package["name"] == consumer_name:
                if package["source"] is not None:
                    raise RuntimeError("external consumer has a non-local root")
            elif package["source"] != CRATES_IO_SOURCE:
                raise RuntimeError(f"external consumer resolved non-crates.io package: {package['name']}")
            if package["name"] in selected:
                selected[package["name"]].add(package["version"])
        if selected != {name: {version} for name, version in selected_versions.items()}:
            raise RuntimeError(f"external consumer resolved the wrong Tool set: {selected}")
        cargo("check", "--locked")
    print("PASS: the exact Tool set compiles in an unpatched crates.io consumer")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "phase", choices=("package", "published-provider", "published-sdk-only", "published-sdk-cohort")
    )
    args = parser.parse_args()
    versions = cohort_versions()
    revision = selected_revision()
    if args.phase == "package":
        verify_local_archives(versions, revision)
        print("PASS: all three normalized cohort archives have the expected versions")
    else:
        names = (PROVIDER,) if args.phase == "published-provider" else (PROVIDER, MACROS, SDK)
        checksums = {
            name: verify_download(
                name, versions[name], versions,
                None if args.phase == "published-sdk-only" and name == PROVIDER else revision,
            )
            for name in names
        }
        verify_consumer(versions, checksums)


if __name__ == "__main__":
    main()
