"""Build an external, source-local Agent lifecycle consumer with public APIs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib


CORE_PACKAGES = (
    "lenso",
    "lenso-app-plan",
    "lenso-contract-authoring",
    "lenso-contract-authoring-macros",
    "lenso-contract-codegen",
    "lenso-contract-runtime",
    "lenso-guest-sdk",
    "lenso-kernel",
    "lenso-native-adapter",
    "lenso-native-adapter-macros",
    "lenso-plugin-authoring",
    "lenso-plugin-control-plane",
    "lenso-runner",
    "lenso-runtime-codec",
)


def run(command: list[str], cwd: Path, log: Path, *, timeout: int = 600) -> None:
    print("+ " + " ".join(command), flush=True)
    with log.open("w") as output:
        result = subprocess.run(
            command, cwd=cwd, stdout=output, stderr=subprocess.STDOUT,
            timeout=timeout, check=False,
        )
    if result.returncode:
        raise RuntimeError(f"exit {result.returncode}: see {log}\n{log.read_text()[-12000:]}")


def revision(root: Path) -> str:
    return subprocess.check_output(
        ["git", "-C", str(root), "rev-parse", "HEAD"], text=True
    ).strip()


def diff_digest(root: Path) -> str:
    patch = subprocess.check_output(["git", "-C", str(root), "diff", "--binary", "HEAD"])
    return hashlib.sha256(patch).hexdigest()


def fixture_digest(source: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted(source.rglob("*")):
        if path.is_file() and path.suffix in {".py", ".rs", ".toml", ".in", ".md", ".lock"}:
            digest.update(str(path.relative_to(source)).encode() + b"\0" + path.read_bytes())
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core-root", required=True, type=Path)
    parser.add_argument("--agent-root", required=True, type=Path)
    parser.add_argument("--work-dir", required=True, type=Path,
                        help="New directory outside either source checkout; retains all evidence.")
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    core, agent, work = (p.resolve() for p in (args.core_root, args.agent_root, args.work_dir))
    os.environ["RUSTUP_TOOLCHAIN"] = tomllib.loads(
        (agent / "rust-toolchain.toml").read_text()
    )["toolchain"]["channel"]
    if any(work == root or root in work.parents for root in (core, agent)):
        parser.error("--work-dir must be outside the source checkouts")
    if work.exists():
        parser.error("--work-dir must be new (existing task state is never overwritten)")

    source = Path(__file__).resolve().parent
    template = (source / "runner/Cargo.toml.in").read_text()
    manifest = tomllib.loads(template)
    paths = {name: core / "crates" / name for name in CORE_PACKAGES}
    for name in manifest.get("dependencies", {}):
        if (agent / "crates" / name / "Cargo.toml").is_file():
            paths[name] = agent / "crates" / name
    for name, path in paths.items():
        if not (path / "Cargo.toml").is_file():
            parser.error(f"missing public package {name}: {path}")
    if template.count("# LENSO_LOCAL_PATCHES") != 1:
        parser.error("runner template must contain one local patch marker")

    shutil.copytree(source, work, ignore=shutil.ignore_patterns("target", "__pycache__", "*.pyc"))
    patch = "[patch.crates-io]\n" + "\n".join(
        f"{name} = {{ path = {json.dumps(str(path))} }}" for name, path in sorted(paths.items())
    )
    runner = work / "runner"
    (runner / "Cargo.toml").write_text(template.replace("# LENSO_LOCAL_PATCHES", patch))
    evidence = {
        "qualification": "source-local public API consumer; not registry/release qualification",
        "core_sha": revision(core),
        "agent_sha": revision(agent),
        "core_tracked_diff_sha256": diff_digest(core),
        "agent_tracked_diff_sha256": diff_digest(agent),
        "fixture_sha256": fixture_digest(source),
        "source_patches": {name: str(path) for name, path in sorted(paths.items())},
        "cargo": subprocess.check_output(["cargo", "--version"], text=True).strip(),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    }
    (work / "source-evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
    os.environ.setdefault("CARGO_BUILD_JOBS", "2")
    os.environ["LENSO_FIXTURE_EVIDENCE_DIR"] = str(work / "evidence")
    offline = ["--offline"] if args.offline else []
    if not (runner / "Cargo.lock").exists():
        run(["cargo", "generate-lockfile", *offline], runner, work / "lock.log")
    run(["cargo", "test", "--locked", *offline, "--test", "lifecycle", "--", "--nocapture"],
        runner, work / "lifecycle.log")
    print((work / "lifecycle.log").read_text()[-8000:])
    print(f"PASS: independent Agent lifecycle consumer; evidence retained at {work}")


if __name__ == "__main__":
    main()
