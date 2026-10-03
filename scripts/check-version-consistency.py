"""Read-only consistency evidence for explicitly selected Lenso source checkouts.

No fetching checkouts, installation, build, publication or version synchronization.
The existing SDK checker remains the only registry/semver implementation.
"""
from __future__ import annotations

import argparse
import ast
from concurrent.futures import ThreadPoolExecutor
import contextlib
from functools import lru_cache
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
REPOSITORIES = {"core": "LioRael/lenso", "js": "LioRael/lenso-js",
                "agent": "LioRael/lenso-agent", "console": "LioRael/lenso-console",
                "console_core": "LioRael/lenso"}


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


sdk = module("starter_prerequisites", HERE / "check-tool-starter-prerequisites.py")


def check(identifier, status, code, message, locations=(), remediation="Review the named source input.", **evidence):
    return {"id": identifier, "status": status, "code": code, "message": message,
            "locations": list(locations), "remediation": remediation, **evidence}


def overall(checks):
    return 1 if any(c["status"] == "fail" for c in checks) else 2 if any(
        c["status"] == "unknown" for c in checks) else 0


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True,
                                   stderr=subprocess.DEVNULL, timeout=30).strip()


def selected_sources(selection):
    roots, evidence, checks = {}, {}, []
    supplied = selection["repositories"]
    for name in ("core", "js", "agent", "console", "console_core"):
        item = supplied.get(name)
        if item is None:
            if name != "console_core":
                checks.append(check("source:" + name, "unknown", "source_not_selected", name + " is not selected",
                                    remediation="Supply its reviewed checkout and full revision; never substitute a cached main."))
            continue
        revision = item.get("revision", "")
        if not re.fullmatch(r"[0-9a-f]{40}", revision):
            checks.append(check("source:" + name, "fail", "full_sha_required", name + " needs a full source SHA"))
            continue
        root = Path(item["path"]).resolve()
        try:
            actual = git(root, "rev-parse", "HEAD")
            origin = git(root, "remote", "get-url", "origin")
            expected = REPOSITORIES[name]
            origin_ok = origin in (f"https://github.com/{expected}", f"https://github.com/{expected}.git",
                                   f"git@github.com:{expected}.git")
            dirty = bool(git(root, "status", "--porcelain"))
            evidence[name] = {"repository": expected, "requested_sha": revision,
                              "observed_sha": actual, "dirty": dirty}
            if actual != revision or not origin_ok:
                checks.append(check("source:" + name, "fail", "source_identity_mismatch",
                                    name + " checkout is not the exact selected source",
                                    remediation="Check out the reviewed full SHA in the correct repository; version equality is insufficient."))
            elif dirty:
                checks.append(check("source:" + name, "unknown", "dirty_source", name + " has uncommitted inputs",
                                    remediation="Commit/review the intended inputs and rerun with their exact SHA."))
            else:
                roots[name] = root
                checks.append(check("source:" + name, "pass", "source_identity_verified", name + " exact clean source verified"))
        except (OSError, subprocess.SubprocessError):
            checks.append(check("source:" + name, "unknown", "source_unavailable", name + " checkout cannot be read",
                                remediation="Fetch the exact visible reviewed ref into a separate checkout; retain pending if unavailable."))
    for item in selection.get("pending_candidates", []):
        checks.append(check("pending:" + item["repository"] + ":" + item["reference"], "unknown", "candidate_pending",
                            "Requested candidate is not qualified by this selection: " + item["reference"],
                            remediation="Resolve the exact visible ref and full SHA, then supply that checkout and rerun.",
                            repository=item["repository"], reason=item["reason"]))
    return roots, evidence, checks


def run_check(identifier, argv, root, locations, remediation, env=None, stdin=None):
    try:
        result = subprocess.run(argv, cwd=root, env=env, input=stdin, capture_output=True, text=True, timeout=90)
    except (OSError, subprocess.TimeoutExpired):
        return check(identifier, "unknown", "checker_unavailable", "Existing checker could not execute", locations, remediation)
    # Retain a digest, not arbitrary child output which may contain local paths.
    return check(identifier, "pass" if result.returncode == 0 else "fail", "owner_check_passed" if result.returncode == 0 else "owner_check_failed",
                 "Existing owner checker " + ("passed" if result.returncode == 0 else "rejected these inputs"), locations,
                 remediation, exit_code=result.returncode,
                 command=[str(v).replace(str(root), "<selected-checkout>") for v in argv],
                 output_sha256=hashlib.sha256((result.stdout + result.stderr).encode()).hexdigest())


def check_cli_context(manifest, js_sha, rust_sha, version):
    """Preflight the existing lenso.cli.npm-candidate.v1 identity, not a new receipt."""
    expected = {"schema": "lenso.cli.npm-candidate.v1", "package": "@lenso/cli",
                "js_source_sha": js_sha, "rust_source_sha": rust_sha, "version": version}
    for field, value in expected.items():
        if manifest.get(field) != value:
            return check("cli:source-pair", "fail", "cli_source_pair_mismatch", "CLI receipt differs at " + field,
                         ["js:.github/scripts/cli-release-artifacts.mjs"],
                         "Regenerate/review the candidate for the exact selected JS and Rust sources; do not accept equal versions as equal source.")
    return check("cli:source-pair", "pass", "cli_source_pair_matches", "CLI receipt names the selected source pair")


def cli_artifact(selection, roots, revisions):
    item = selection.get("cli_candidate")
    hint = "Supply the existing release-cli-npm candidate archive, manifest and public artifact metadata; no new build is run by this checker."
    if not item or not {"core", "js"}.issubset(roots):
        return [check("cli:artifact", "unknown", "artifact_not_supplied", "CLI exact source-pair artifact is not verified", remediation=hint)]
    manifest = json.loads(Path(item["manifest"]).read_text())
    package = json.loads((roots["js"] / "packages/lenso-cli/package.json").read_text())
    js_sha, rust_sha = (revisions[n]["observed_sha"] for n in ("js", "core"))
    identity = check_cli_context(manifest, js_sha, rust_sha, package["version"])
    if identity["status"] != "pass":
        return [identity]
    script = roots["js"] / ".github/scripts/cli-release-artifacts.mjs"
    args = ["node", str(script), "verify-candidate", item["archive"], item["manifest"], js_sha, rust_sha,
            str(item["run_id"]), str(item["run_attempt"]), package["version"], item["reviewed_archive_sha256"],
            str(item["candidate_artifact_id"]), item["candidate_artifact_digest"]]
    return [identity, run_check("cli:artifact", args, roots["js"], ["js:.github/scripts/cli-release-artifacts.mjs"],
                                hint, stdin=Path(item["artifact_metadata"]).read_text())]


def owner_checks(selection, roots, revisions, codegen):
    checks = []
    if "core" in roots:
        path = roots["core"] / ".github/scripts/check-fixture-inputs.py"
        try:
            owner = module("core_fixture_inputs", path)
            with contextlib.redirect_stdout(io.StringIO()):
                owner.check_web_cohort(roots["core"])
            checks.append(check("core:web-template", "pass", "owner_check_passed", "Core Web template and declared source cohort agree",
                                ["core:.github/scripts/check-fixture-inputs.py"]))
        except (OSError, AttributeError, ValueError, RuntimeError, IndexError, KeyError, SyntaxError):
            checks.append(check("core:web-template", "fail", "web_template_drift", "Core template/source check failed",
                                ["core:crates/lenso-engine-app/src/plugin/scaffold.rs"],
                                "Run check_web_cohort from Core's existing checker; update its template and independent assertions together."))
    if "agent" in roots:
        root = roots["agent"]
        version = sdk.toml(root / "apps/lenso-agent-tui/Cargo.toml")["package"]["version"]
        checks.append(run_check("agent:binary-release-version", ["bash", "scripts/check-release-version.sh", version], root,
                                ["agent:scripts/check-release-version.sh"], "Align only the four Agent binary release manifests and installer per the existing release script."))
    if "js" in roots:
        checks.append(run_check("js:generated-capabilities", ["node", "packages/lenso-bun/scripts/capabilities.mjs", "check"], roots["js"],
                                ["js:packages/lenso-bun/capabilities.lock.json"],
                                "Review the lock's individual source revisions and regenerate using its owner workflow; this check does not replace source-origin verification.",
                                env={**os.environ, "LENSO_CONTRACT_CODEGEN": str(codegen)}))
    if "console" in roots:
        root = roots["console"]
        checks.append(run_check("console:sdk-projections", ["node", "tooling/check-console-sdk.mjs"], root,
                                ["console:tooling/check-console-sdk.mjs"],
                                "Regenerate the SDK copies with the existing Console owner command and review the changed projections."))
        script = root / ".github/scripts/check-development-host-source.py"
        hint = "Supply the exact Core checkout selected by the Console qualification workflow; run its existing facility/runtime source checker."
        if script.is_file() and "console_core" in roots:
            with tempfile.TemporaryDirectory(prefix="lenso-consistency-") as tmp:
                for mode in ("facility", "runtime"):
                    seed = selection.get("console_generated_seed")
                    if mode == "runtime" and not seed:
                        checks.append(check("console:runtime-source", "unknown", "generated_input_not_supplied",
                                            "Generated Host input provenance remains unverified", remediation=hint))
                        continue
                    args = [sys.executable, str(script), mode, "--core", str(roots["console_core"]), "--record", str(Path(tmp) / (mode + ".json"))]
                    if mode == "runtime":
                        args += ["--seed", seed]
                    env = {**os.environ, "CORE_REPOSITORY": REPOSITORIES["core"], "CORE_REVISION": revisions["console_core"]["observed_sha"]}
                    checks.append(run_check("console:" + mode + "-source", args, root, ["console:.github/scripts/check-development-host-source.py"], hint, env=env))
        else:
            checks.append(check("console:source-contract", "unknown", "source_checker_not_selected",
                                "This Console selection has no usable exact source qualification inputs", remediation=hint))
    return checks


def console_distribution(root, offline):
    """Respect the existing launcher's deliberate version rewrite and Agent pin."""
    paths = ["tooling/distribution/package-console.mjs", "tooling/distribution/package-agent.mjs"]
    version = json.loads((root / "apps/shell/package.json").read_text())["version"]
    for path in paths:
        source = (root / path).read_text()
        if any(token not in source for token in ('apps/shell/package.json', 'manifest.version = version;', 'manifest.optionalDependencies = Object.fromEntries(')):
            return [], [check("console:distribution-version", "unknown", "packager_contract_changed",
                              "Launcher version rewrite needs renewed review", paths,
                              "Review the existing packagers before deriving their published package versions.")], {}
    checks = [check("console:distribution-version", "pass", "derived_launcher_versions",
                    "Launcher and platform output versions come from apps/shell, not the private template version",
                    ["console:apps/shell/package.json", *["console:" + p for p in paths]],
                    "Keep the private template version independent; validate produced archives before release.", derived_version=version)]
    requirements = [{"name": "@lenso/" + package + suffix, "ecosystem": "npm", "requirement": "=" + version,
                     "owner": "Console distribution output", "role": "source_version", "locations": ["console:apps/shell/package.json", "console:" + paths[0 if package == "console" else 1]]}
                    for package in ("agent", "agent-native", "console") for suffix in ("", "-darwin-arm64", "-linux-x64")]
    pin = json.loads((root / "tooling/distribution/agent-release.json").read_text())
    required = {f"{exe}-v{pin['version']}-{target}.tar.gz" for exe in
                ("lenso-agent", "lenso-agent-cli", "lenso-agent-acp", "lenso-agent-web", "lenso-agent-console-web")
                for target in ("darwin-aarch64", "linux-x86_64")}
    valid = (pin["repository"] == REPOSITORIES["agent"] and re.fullmatch(r"\d+\.\d+\.\d+", pin["version"])
             and required.issubset(pin["assets"]) and all(re.fullmatch(r"[0-9a-f]{64}", pin["assets"][name]) for name in required))
    checks.append(check("console:agent-release-pin", "pass" if valid else "fail", "agent_release_pin_shape",
                        "Console's independently pinned Agent release must name ten checksummed assets",
                        ["console:tooling/distribution/agent-release.json"], "Review the exact Agent release and update the owner pin, never substitute the current Agent checkout implicitly."))
    if not valid:
        return requirements, checks, {"state": "invalid_pin"}
    evidence, body = sdk.fetch(f"https://api.github.com/repos/{REPOSITORIES['agent']}/releases/tags/v{pin['version']}", offline)
    if body is None:
        checks.append(check("console:agent-release-metadata", "fail" if evidence["state"] == "package_not_found" else "unknown", evidence["state"], "Pinned Agent release assets are not currently verified",
                            remediation="Restore public GitHub read access and rerun; do not treat a transport failure as a missing version."))
    else:
        try:
            release = json.loads(body)
            assets = {a["name"]: a for a in release["assets"]}
            matches = release["tag_name"] == "v" + pin["version"] and required.issubset(assets)
        except (ValueError, KeyError, TypeError):
            matches = False
        checks.append(check("console:agent-release-metadata", "pass" if matches else "fail", "release_assets_present" if matches else "release_assets_missing",
                            "Pinned release tag and asset names checked against public GitHub metadata",
                            ["console:tooling/distribution/agent-release.json"], "Have the Agent release owner reconcile the exact release assets."))
    return requirements, checks, evidence


def source_fixture_requirements(root, requirements):
    """Record explicit source-local verifier patches; do not run a verifier."""
    fixtures = []
    scripts = list((root / "examples").glob("*/verify-foundation.py"))
    lifecycle = root / "examples/agent-foundation-lifecycle/verify.py"
    if lifecycle.is_file():
        scripts.append(lifecycle)
    for script in sorted(scripts):
        readme = script.with_name("README.md")
        if not readme.is_file() or not any(label in readme.read_text() for label in ("source-local", "source/local")):
            continue
        tree = ast.parse(script.read_text())
        if not any(isinstance(n, ast.Constant) and isinstance(n.value, str) and n.value.rstrip("\n") == "[patch.crates-io]" for n in ast.walk(tree)):
            continue
        names = set()
        for node in tree.body:
            if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "paths" for t in node.targets) and isinstance(node.value, ast.Dict):
                names.update(k.value for k in node.value.keys if isinstance(k, ast.Constant) and isinstance(k.value, str))
            if isinstance(node, ast.For) and isinstance(node.target, ast.Name) and node.target.id == "name" and isinstance(node.iter, (ast.Tuple, ast.List)):
                assigns_paths = any(isinstance(n, ast.Assign) and any(isinstance(t, ast.Subscript) and isinstance(t.value, ast.Name) and t.value.id == "paths" for t in n.targets) for n in node.body)
                if assigns_paths:
                    names.update(k.value for k in node.iter.elts if isinstance(k, ast.Constant) and isinstance(k.value, str))
        if script == lifecycle:
            # Recognize only this owner's current explicit patch construction.
            # AST shape matching reads names; it does not interpret Python or
            # qualify the copied consumer, dependency graph, or API versions.
            nodes = {ast.dump(n) for n in ast.walk(tree)}
            shapes = [
                'template = (source / "runner/Cargo.toml.in").read_text()',
                'manifest = tomllib.loads(template)',
                'paths = {name: core / "crates" / name for name in CORE_PACKAGES}',
                'for name in manifest.get("dependencies", {}):\n if (agent / "crates" / name / "Cargo.toml").is_file():\n  paths[name] = agent / "crates" / name',
                'patch = "[patch.crates-io]\\n" + "\\n".join(f"{name} = {{ path = {json.dumps(str(path))} }}" for name, path in sorted(paths.items()))',
                'runner = work / "runner"',
                '(runner / "Cargo.toml").write_text(template.replace("# LENSO_LOCAL_PATCHES", patch))',
            ]
            if not all(ast.dump(ast.parse(shape).body[0]) in nodes for shape in shapes):
                raise ValueError("unrecognized lifecycle source patch construction")
            for node in tree.body:
                if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "CORE_PACKAGES" for t in node.targets):
                    names.update(ast.literal_eval(node.value))
            template = script.parent / "runner/Cargo.toml.in"
            names.update(name for name in tomllib.loads(template.read_text()).get("dependencies", {})
                         if (root / "crates" / name / "Cargo.toml").is_file())
        if names:
            fixtures.append((script.parent.relative_to(root).as_posix() + "/", names, script.relative_to(root).as_posix()))
    result = []
    for item in requirements:
        local = [script for prefix, names, script in fixtures if item["name"] in names and
                 item.get("locations") and all(p.startswith(prefix) for p in item["locations"])]
        result.append({**item, "role": "source_fixture", "source_override_verifiers": local} if local else item)
    return result


def probe_requirements(requirements, offline):
    selected = {}
    for item in requirements:
        key = (item["ecosystem"], item["name"], item["requirement"])
        previous = selected.get(key, {})
        locations = sorted(set(previous.get("locations", []) + item.get("locations", [])))
        roles = sorted(set(previous.get("roles", []) + [item.get("role", "dependency")]))
        owners = sorted(set(previous.get("owners", []) + [item.get("owner", "explicit prerequisite")]))
        verifiers = sorted(set(previous.get("source_override_verifiers", []) + item.get("source_override_verifiers", [])))
        if key not in selected or item.get("archive_identity"):
            selected[key] = dict(item)
        selected[key].update(locations=locations, roles=roles, owners=owners)
        if verifiers:
            selected[key]["source_override_verifiers"] = verifiers
    groups = {}
    for item in selected.values():
        groups.setdefault((item["ecosystem"], item["name"]), []).append(item)
    original = sdk.fetch
    # One fresh public response per URL in this invocation. No persisted cache.
    sdk.fetch = lru_cache(maxsize=None)(original)
    try:
        with ThreadPoolExecutor(max_workers=4) as pool:
            batches = list(pool.map(lambda items: [sdk.registry_probe(v, offline) for v in items], groups.values()))
    finally:
        sdk.fetch = original
    return [row for batch in batches for row in batch]


def registry_check(item):
    state = item["state"]
    status = sdk.exit_status([], [item])
    if state == "unsupported_requirement":
        status = 2  # A broader inventory must not guess unsupported range syntax.
    if state in ("version_absent", "package_not_found") and item.get("roles") and set(item["roles"]).issubset({"source_version", "source_fixture"}):
        return check("registry:source:" + item["name"] + ":" + item["requirement"], "unknown", "source_version_not_published",
                     item["name"] + " source/fixture version is not published; this alone is not a broken registry consumer pin",
                     item.get("locations", []), "Review the owning release plan or explicit source-local fixture verifier. Source-patched tests do not establish published consumption or authorize publication.")
    message = item["name"] + " " + item["requirement"] + ": " + state
    hint = ("Have the package owner review/publish the required supported version; never silently downgrade." if state in ("version_absent", "package_not_found") else
            "This version is occupied but withdrawn; do not upload it again. Owner action needs separate authorization." if state == "version_yanked" else
            "Restore registry access or supply a supported exact requirement; unknown availability is not a pass." if status == 2 else
            "Review the returned archive/metadata evidence; equal versions do not prove API or source identity.")
    return check("registry:" + item["ecosystem"] + ":" + item["name"] + ":" + item["requirement"],
                 {0: "pass", 1: "fail", 2: "unknown"}[status], state, message, item.get("locations", []), hint)


def report(selection, codegen, offline=False):
    if selection.get("schema") != "lenso.version-consistency-selection@1":
        raise ValueError("unsupported selection schema")
    roots, revisions, checks = selected_sources(selection)
    inventory, requirements = [], []
    distribution = {}
    scanner = module("manifest_inputs", HERE / "version_manifest_inputs.py")
    docs = module("doc_entrypoints", HERE / "version_doc_entrypoints.py")
    for name, root in roots.items():
        if name == "console_core":
            continue
        packages, dependencies, local_checks = scanner.scan_repository(root, name, sdk.compatible)
        inventory.extend(packages)
        if name == "agent":
            dependencies = source_fixture_requirements(root, dependencies)
        dependencies = [{**d, "locations": [name + ":" + p for p in d.get("locations", [])]} for d in dependencies]
        if name == "console":
            # Existing packagers replace these private template requirements.
            dependencies = [d for d in dependencies if d.get("owner") not in ("@lenso/agent", "@lenso/console")]
            derived, distribution_checks, distribution = console_distribution(root, offline)
            dependencies.extend(derived)
            checks.extend(distribution_checks)
        requirements.extend(dependencies)
        checks.extend({**c, "locations": [name + ":" + p for p in c["locations"]]} for c in local_checks)
        checks.extend({**row, "id": name + ":" + row["id"], "locations": [name + ":" + p for p in row["locations"]]}
                      for row in docs.check_documents(root, selection.get("documents", {}).get(name, [])))
    starter = {"state": "source_not_selected"}
    if {"agent", "core"}.issubset(roots):
        starter = sdk.build_report(roots["agent"], roots["core"], codegen, offline=True)
        for error in starter["source_errors"]:
            checks.append(check("sdk:source", "fail", "sdk_source_mismatch", error,
                                ["core:crates/lenso-engine-app/src/plugin/scaffold.rs", "agent:packages/lenso-agent-tool-sdk/package.json"],
                                "Use the existing SDK prerequisite report to reconcile template, manifest and generated contract inputs."))
        requirements.extend({**r, "locations": ["agent:scripts/check-tool-starter-prerequisites.py"]} for r in starter["registry"])
        starter = {k: v for k, v in starter.items() if k not in ("schema", "registry", "exit_code")}
        starter["registry_evidence"] = "Collected once in the unified top-level registry array."
    checks.extend(owner_checks(selection, roots, revisions, codegen))
    checks.extend(cli_artifact(selection, roots, revisions))
    # Read explicit CI generator/install pins without requiring current libraries
    # to share the generator version or the same release cadence.
    if "agent" in roots:
        text = (roots["agent"] / "scripts/check.sh").read_text()
        for name, version in re.findall(r"cargo install (lenso[\w-]*) --version ['\"]?(=?[0-9]+\.[0-9]+\.[0-9]+)", text):
            requirements.append({"name": name, "ecosystem": "cargo", "requirement": version,
                                 "owner": "Agent CI tool", "locations": ["agent:scripts/check.sh"]})
    registry = probe_requirements(requirements, offline)
    checks.extend(map(registry_check, registry))
    status = overall(checks)
    return {"schema": "lenso.version-consistency-report@1", "exit_code": status,
            "status": {0: "checked_scope_passed", 1: "blocked", 2: "pending"}[status],
            "sources": revisions, "package_inventory": inventory, "checks": checks,
            "registry": registry, "tool_starter": starter, "console_agent_release": distribution,
            "registry_mode": "offline_unchecked" if offline else "fresh public URL observations, same-run memory reuse only",
            "not_proven": ["publication authorization", "unselected or unavailable candidates", "full dependency resolution / ABI compatibility",
                           "Console Agent binary archive integrity / execution",
                           "native/Wasm/Workers execution qualification", "unassessed document commands", "upstream origin of vendored JS contract snapshots"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--selection", required=True, type=Path)
    parser.add_argument("--codegen", required=True, type=Path)
    parser.add_argument("--offline", action="store_true")
    args = parser.parse_args()
    try:
        result = report(json.loads(args.selection.read_text()), args.codegen.resolve(), args.offline)
    except (OSError, ValueError, KeyError, TypeError, SyntaxError, subprocess.SubprocessError) as error:
        result = {"schema": "lenso.version-consistency-report@1", "exit_code": 1, "status": "blocked",
                  "checks": [check("input", "fail", "invalid_input", "Consistency inputs could not be read (" + type(error).__name__ + ")",
                                   remediation="Review selection paths/schema and required manifests; no availability or compatibility pass was produced.")]}
    print(json.dumps(result, indent=2))
    raise SystemExit(result["exit_code"])


if __name__ == "__main__":
    main()
