"""Run the lifecycle consumer against Agent's exact Core pin; retain source-local proof."""
from __future__ import annotations

import argparse
import ast
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib

CORE_REPOSITORY = "https://github.com/LioRael/lenso"


def core_revision(agent: Path) -> str:
    patches = tomllib.loads((agent / "Cargo.toml").read_text())["patch"]["crates-io"]
    try:
        tree = ast.parse((agent / "examples/agent-foundation-lifecycle/verify.py").read_text())
        declarations = [node.value for node in tree.body if isinstance(node, ast.Assign)
                        and any(isinstance(target, ast.Name) and target.id == "CORE_PACKAGES" for target in node.targets)]
        names = ast.literal_eval(declarations[0]) if len(declarations) == 1 else None
        if not isinstance(names, (tuple, list)) or not names or any(not isinstance(name, str) for name in names):
            raise ValueError("invalid literal")
    except (SyntaxError, TypeError, ValueError) as error:
        raise ValueError("Fixture must declare one literal CORE_PACKAGES list or tuple") from error
    for name in names:
        spec = patches.get(name)
        if not isinstance(spec, dict) or spec.get("git") not in (CORE_REPOSITORY, CORE_REPOSITORY + ".git"):
            raise ValueError(f"Fixture Core package {name} must have an explicit official Core Git patch")
    core = {name: spec for name, spec in patches.items() if isinstance(spec, dict)
            and spec.get("git") in (CORE_REPOSITORY, CORE_REPOSITORY + ".git")}
    revisions = {spec.get("rev") for spec in core.values()}
    if "lenso" not in core or len(revisions) != 1:
        raise ValueError("Core patch entries must select one complete revision, including lenso")
    revision = revisions.pop()
    if not isinstance(revision, str) or not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("Core patch revision must be a full 40-character SHA")
    return revision


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def checked_checkout(root: Path, label: str) -> str:
    if Path(git(root, "rev-parse", "--show-toplevel")).resolve() != root:
        raise ValueError(f"{label} source path must be the Git checkout root")
    if git(root, "status", "--porcelain", "--untracked-files=all"):
        raise ValueError(f"{label} checkout is dirty; use a clean exact checkout without resetting existing work")
    return git(root, "rev-parse", "HEAD")


def checked_core(core: Path, expected: str) -> str:
    actual = checked_checkout(core, "Core")
    if actual != expected:
        raise ValueError(f"Core HEAD {actual} does not match Agent pin {expected}; select a separate exact checkout")
    return actual


def outside(path: Path, sources: tuple[Path, ...]) -> None:
    if any(path == source or source in path.parents for source in sources):
        raise ValueError("Evidence directory must be outside both source checkouts")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent-root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--core-root", type=Path, default=os.environ.get("LENSO_CORE_ROOT"),
                        help="Existing clean checkout at the pin; otherwise fetch that public SHA into the evidence directory.")
    parser.add_argument("--evidence-root", type=Path, default=os.environ.get("LENSO_FOUNDATION_LIFECYCLE_ROOT"),
                        help="New external directory; never reuse or overwrite existing evidence.")
    parser.add_argument("--offline", action="store_true", help="Requires --core-root and cached locked dependencies.")
    args = parser.parse_args()
    if args.offline and args.core_root is None:
        parser.error("--offline requires --core-root (or LENSO_CORE_ROOT)")
    agent = args.agent_root.resolve()
    evidence = None
    receipt = {"qualification": "source-local public API consumer; not registry or release qualification"}
    try:
        expected = core_revision(agent)
        agent_sha = checked_checkout(agent, "Agent")
        event_sha = os.environ.get("GITHUB_SHA")
        if event_sha is not None and (not re.fullmatch(r"[0-9a-f]{40}", event_sha) or event_sha != agent_sha):
            raise ValueError("Agent HEAD must match the full GITHUB_SHA for this CI attempt")
        supplied_core = args.core_root.resolve() if args.core_root else None
        evidence = (args.evidence_root.resolve() if args.evidence_root else
                    Path(tempfile.mkdtemp(prefix="lenso-agent-lifecycle-")) / "proof")
        outside(evidence, (agent,) + ((supplied_core,) if supplied_core else ()))
        evidence.mkdir(parents=True, exist_ok=False)
        print(f"Foundation lifecycle evidence: {evidence}", flush=True)
        receipt.update(agent_sha=agent_sha, core_expected_sha=expected,
                       core_repository=CORE_REPOSITORY, status="preparing")
        core = supplied_core or evidence / "core"
        if supplied_core is None:
            core.mkdir()
            with (evidence / "core-fetch.log").open("w") as output:
                for command in (["git", "init", "--quiet", str(core)],
                                ["git", "-C", str(core), "-c", "credential.helper=", "fetch", "--no-tags", "--depth=1", CORE_REPOSITORY, expected],
                                ["git", "-C", str(core), "checkout", "--quiet", "--detach", "FETCH_HEAD"]):
                    subprocess.run(command, check=True, stdout=output, stderr=subprocess.STDOUT,
                                   env={**os.environ, "GIT_TERMINAL_PROMPT": "0"}, timeout=180)
        receipt["core_sha"] = checked_core(core, expected)
        work = evidence / "consumer"
        outside(work, (agent, core))
        command = [sys.executable, str(agent / "examples/agent-foundation-lifecycle/verify.py"),
                   "--core-root", str(core), "--agent-root", str(agent), "--work-dir", str(work)]
        if args.offline:
            command.append("--offline")
        receipt.update(command=command, status="running")
        (evidence / "lifecycle-gate.json").write_text(json.dumps(receipt, indent=2) + "\n")
        result = subprocess.run(command, cwd=agent, check=False)
        receipt.update(status="passed" if result.returncode == 0 else "failed", verifier_exit_code=result.returncode)
        return result.returncode
    except (KeyError, OSError, ValueError, subprocess.SubprocessError) as error:
        receipt.update(status="failed", error=str(error))
        print(f"Foundation lifecycle gate failed: {error}", file=sys.stderr)
        return 2
    finally:
        if evidence is not None and receipt.get("agent_sha"):
            (evidence / "lifecycle-gate.json").write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    raise SystemExit(main())
