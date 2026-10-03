"""Tracked package inventories and local dependency/lock checks; no network or build."""
from __future__ import annotations

import json
from fnmatch import fnmatchcase
from pathlib import Path
import re
import subprocess
import tomllib

SECTIONS = ("dependencies", "dev-dependencies", "build-dependencies")
JS_SECTIONS = ("dependencies", "devDependencies", "optionalDependencies", "peerDependencies")
MAX_BYTES = 8 * 1024 * 1024


def first_party(name):
    return isinstance(name, str) and (name == "lenso" or name.startswith(("lenso-", "@lenso/")))


def selected(path, patterns):
    """Bounded current workspace globs: literal paths and per-segment * / ?."""
    if not isinstance(patterns, list) or any(not isinstance(p, str) or not p or
            any(c in p for c in "![]{}\\") or "**" in p or p.startswith("/") or ".." in p.split("/") for p in patterns):
        raise ValueError("unsupported workspace patterns")
    parts = path.split("/")
    return any(len(parts) == len(p.removeprefix("./").rstrip("/").split("/")) and
               all(fnmatchcase(a, b) for a, b in zip(parts, p.removeprefix("./").rstrip("/").split("/"))) for p in patterns)


def bun_json(text):
    """Remove only out-of-string trailing commas; reject duplicate keys and comments."""
    if len(text.encode()) > MAX_BYTES:
        raise ValueError("lock exceeds size bound")
    chars, quoted, escaped = list(text), False, False
    for i, char in enumerate(text):
        if quoted:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                quoted = False
        elif char == '"':
            quoted = True
        elif char == ",":
            j = i + 1
            while j < len(text) and text[j].isspace():
                j += 1
            if j < len(text) and text[j] in "}]":
                chars[i] = " "
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate key")
            result[key] = value
        return result
    return json.loads("".join(chars), object_pairs_hook=unique)


def scan_repository(root: Path, repo_name: str, compatible_callable):
    root = root.resolve()
    inventory, requirements, checks = [], [], []
    def check(status, code, message, locations, remediation):
        checks.append(dict(id=f"{repo_name}:manifest:{len(checks)}", status=status, code=code,
                           message=message, locations=list(dict.fromkeys(locations)), remediation=remediation))
    def unknown(code, locations):
        check("unknown", code, "Input cannot be resolved unambiguously within the tracked repository.",
              locations, "Correct the input or extend this bounded checker with a reviewed regression test.")
    tracked = set(subprocess.check_output(["git", "-C", str(root), "ls-files", "-z"]).decode().split("\0")) - {""}
    def read(relative):
        path = root / relative
        if relative not in tracked or not path.resolve().is_relative_to(root) or path.is_symlink():
            raise ValueError("untracked or external input")
        if path.stat().st_size > MAX_BYTES:
            raise ValueError("input exceeds size bound")
        return path.read_text()
    manifests, locks = {}, {}
    for relative in sorted(tracked):
        filename = Path(relative).name
        if filename not in ("Cargo.toml", "package.json", "Cargo.lock", "bun.lock"):
            continue
        try:
            data = (tomllib.loads(read(relative)) if filename.startswith("Cargo.") else bun_json(read(relative)))
            if not isinstance(data, dict):
                raise ValueError("expected object")
            if filename == "Cargo.toml":
                for key in ("package", "workspace", "target"):
                    if not isinstance(data.get(key, {}), dict):
                        raise ValueError("expected table")
                if any(not isinstance(v, dict) for v in data.get("target", {}).values()):
                    raise ValueError("expected target tables")
                if any(not isinstance(data.get("workspace", {}).get(k, {}), dict) for k in ("package", "dependencies")):
                    raise ValueError("expected workspace tables")
            if filename == "package.json" and not isinstance(data.get("publishConfig", {}), dict):
                raise ValueError("expected publishConfig object")
            (locks if filename in ("Cargo.lock", "bun.lock") else manifests)[relative] = data
        except (OSError, ValueError, UnicodeError):
            unknown("manifest_parse_unknown", [relative])
    def location(path):
        resolved = path.resolve()
        return resolved.relative_to(root).as_posix() if resolved.is_relative_to(root) else None
    def candidate_workspace(relative, data):
        explicit = data.get("package", {}).get("workspace")
        if explicit is not None:
            candidate = location(root / Path(relative).parent / explicit / "Cargo.toml") if isinstance(explicit, str) else None
            return candidate if candidate in manifests and "workspace" in manifests[candidate] else None
        parent = Path(relative).parent
        while True:
            candidate = (parent / "Cargo.toml").as_posix()
            if "workspace" in manifests.get(candidate, {}):
                return candidate
            if parent == Path("."):
                return None
            parent = parent.parent
    owners, member_sets = {}, {}
    def workspace(relative, data):
        if relative in owners:
            return owners[relative]
        candidate = candidate_workspace(relative, data)
        owner = relative if candidate is None and "workspace" not in data.get("package", {}) else candidate
        if candidate == relative:
            try:
                selected(".", data["workspace"].get("members", []))
                selected(".", data["workspace"].get("exclude", []))
            except ValueError:
                owner = None
        if candidate and candidate != relative:
            ws = manifests[candidate]["workspace"]
            base = (root / candidate).parent
            try:
                if candidate not in member_sets:
                    paths = {p: (root / p).parent.relative_to(base).as_posix() for p in manifests
                             if p.endswith("Cargo.toml") and (root / p).is_relative_to(base)}
                    excluded = {p for p, local in paths.items() if selected(local, ws.get("exclude", []))}
                    members = {candidate} | {p for p, local in paths.items() if selected(local, ws.get("members", [])) and p not in excluded}
                    pending = list(members)
                    while pending:
                        member = pending.pop()
                        source = manifests[member]
                        groups = [source.get(k, {}) for k in SECTIONS] + [t.get(k, {}) for t in source.get("target", {}).values() for k in SECTIONS]
                        for group in groups:
                            if not isinstance(group, dict):
                                raise ValueError("unknown dependency table")
                            for alias, spec in group.items():
                                origin = member
                                if isinstance(spec, dict) and spec.get("workspace") is True:
                                    spec, origin = ws.get("dependencies", {}).get(alias), candidate
                                if not isinstance(spec, dict) or not isinstance(spec.get("path"), str):
                                    continue
                                target = location(root / Path(origin).parent / spec["path"] / "Cargo.toml")
                                if target in paths and target not in excluded | members and candidate_workspace(target, manifests[target]) == candidate:
                                    members.add(target)
                                    pending.append(target)
                    member_sets[candidate] = members, excluded
                members, excluded = member_sets[candidate]
                owner = candidate if relative in members else relative if relative in excluded and "workspace" not in data.get("package", {}) else None
            except ValueError:
                owner = None
        owners[relative] = owner
        if owner is None:
            unknown("cargo_workspace_ownership_unknown", [relative] + ([candidate] if candidate else []))
        return owner
    def field(relative, data, key, default=None):
        value = data.get("package", {}).get(key, default)
        if isinstance(value, dict) and value.get("workspace") is True:
            ws = workspace(relative, data)
            inherited = manifests.get(ws, {}).get("workspace", {}).get("package", {}).get(key)
            if inherited is None:
                unknown("workspace_package_inheritance_unknown", [relative])
            return inherited
        return value
    def match(version, requirement, ecosystem):
        if not isinstance(version, str) or not re.fullmatch(r"\d+\.\d+\.\d+", version):
            return None
        try:
            return compatible_callable(version, requirement, ecosystem)
        except (ValueError, TypeError):
            return None
    def require(name, version, ecosystem, owner, locations, role="dependency"):
        if match("0.0.0", version, ecosystem) is None:
            unknown("dependency_requirement_unknown", locations)
        requirements.append(dict(name=name, ecosystem=ecosystem, requirement=version, owner=owner, locations=locations, role=role))
    dependencies = {}
    for relative, data in manifests.items():
        cargo = relative.endswith("Cargo.toml")
        ecosystem = "cargo" if cargo else "npm"
        package = data.get("package", {}) if cargo else data
        if not isinstance(package, dict):
            unknown("package_shape_unknown", [relative])
            continue
        name = package.get("name")
        version = field(relative, data, "version") if cargo else data.get("version")
        publish = field(relative, data, "publish", True) if cargo else not data.get("private", False)
        publishable = bool(publish) and isinstance(name, str) and isinstance(version, str)
        row = dict(repo=repo_name, path=relative, name=name, version=version, ecosystem=ecosystem,
                   publishable=publishable, dependencies=[])
        if name:
            inventory.append(row)
        custom = (isinstance(publish, list) and "crates-io" not in publish if cargo
                  else data.get("publishConfig", {}).get("registry", "https://registry.npmjs.org/").rstrip("/") != "https://registry.npmjs.org")
        if publishable and first_party(name):
            if custom:
                unknown("custom_registry_unknown", [relative])
            else:
                require(name, "=" + version, ecosystem, name, [relative], role="source_version")
        groups = [(relative, data.get(section, {})) for section in (SECTIONS if cargo else JS_SECTIONS)]
        if cargo:
            groups += [(relative, target.get(section, {})) for target in data.get("target", {}).values() for section in SECTIONS]
            groups += [(relative, data.get("workspace", {}).get("dependencies", {}))]
        direct = dependencies.setdefault(relative, [])
        for group_origin, group in groups:
            if not isinstance(group, dict):
                unknown("dependency_shape_unknown", [relative])
                continue
            for alias, raw in group.items():
                origin = group_origin
                spec = dict(raw) if isinstance(raw, dict) else {"version": raw}
                inherited = spec.get("workspace") is True
                if cargo and inherited:
                    ws = workspace(relative, data)
                    raw = manifests.get(ws, {}).get("workspace", {}).get("dependencies", {}).get(alias)
                    spec = dict(raw) if isinstance(raw, dict) else {"version": raw}
                    origin = ws or relative
                dep = spec.get("package", alias)
                if not first_party(dep):
                    continue
                if "git" in spec and "path" in spec:
                    unknown("dependency_source_ambiguous", [relative, origin])
                    continue
                requirement = spec.get("version")
                source = ("git" if "git" in spec else "path" if "path" in spec else
                          "workspace" if isinstance(requirement, str) and requirement.startswith("workspace:") else
                          "path" if isinstance(requirement, str) and requirement.startswith(("file:", "link:")) else
                          "git" if isinstance(requirement, str) and requirement.startswith(("git", "github:")) else "registry")
                locations = list(dict.fromkeys([relative, origin]))
                item = dict(name=dep, requirement=requirement, source=source, inherited=inherited, locations=locations)
                direct.append(item)
                row["dependencies"].append(item)
                if source == "registry":
                    if not isinstance(requirement, str) or "registry" in spec and spec["registry"] != "crates-io":
                        unknown("dependency_requirement_unknown", locations)
                    else:
                        require(dep, requirement, ecosystem, name or f"{repo_name}:{relative}", locations)
                else:
                    check("pass", "dependency_source_recorded", f"{dep} uses {source}; this is not registry availability evidence.",
                          locations, "Use a registry check for any separately declared published requirement.")
                if cargo and source == "path" and "path" in spec:
                    if not isinstance(spec["path"], str):
                        unknown("path_dependency_target_unknown", locations)
                        continue
                    target = location(root / Path(origin).parent / spec["path"] / "Cargo.toml")
                    target_data = manifests.get(target)
                    if not target_data or target_data.get("package", {}).get("name") != dep:
                        unknown("path_dependency_target_unknown", locations)
                    elif requirement is not None:
                        okay = match(field(target, target_data, "version"), requirement, ecosystem)
                        check("unknown" if okay is None else "pass" if okay else "fail", "path_dependency_version",
                              f"{dep}: declared {requirement} versus tracked target {field(target, target_data, 'version')}.",
                              locations + [target], "Align the path dependency requirement with its target package's independent version.")
    for relative, data in manifests.items():
        cargo = relative.endswith("Cargo.toml")
        owner = workspace(relative, data) if cargo else relative
        if owner is None:
            continue
        lockpath = (Path(owner).parent / ("Cargo.lock" if cargo else "bun.lock")).as_posix()
        if lockpath not in locks:
            continue
        lock = locks[lockpath]
        if cargo:
            entries = lock.get("package")
            if not isinstance(entries, list) or any(not isinstance(x, dict) for x in entries):
                unknown("cargo_lock_shape_unknown", [lockpath])
                continue
            for dep in dependencies.get(relative, []):
                if not isinstance(dep["requirement"], str):
                    continue
                matches = [match(x.get("version"), dep["requirement"], "cargo") for x in entries if x.get("name") == dep["name"]]
                syntax = match("0.0.0", dep["requirement"], "cargo")
                status = "unknown" if syntax is None else "pass" if True in matches else "unknown" if None in matches else "fail"
                check(status, "cargo_lock_requirement", f"{dep['name']}: selected workspace/package Cargo.lock must contain a version compatible with {dep['requirement']}.",
                      dep["locations"] + [lockpath], "Refresh this lock with the repository's approved Cargo command; preserve valid Git and registry entries.")
        else:
            workspaces, packages = lock.get("workspaces"), lock.get("packages")
            if not isinstance(workspaces, dict) or not isinstance(packages, dict):
                unknown("bun_lock_shape_unknown", [lockpath])
                continue
            if "" not in workspaces:
                unknown("bun_workspace_unknown", [relative, lockpath])
            try:
                base = (root / relative).parent
                selected(".", data.get("workspaces", []))
                expected = {p: (root / p).parent.relative_to(base).as_posix() for p in manifests
                            if p.endswith("package.json") and (root / p).is_relative_to(base)}
                expected = {p: member for p, member in expected.items() if p != relative and selected(member, data.get("workspaces", []))}
                for manifestpath, member in expected.items():
                    check("pass" if member in workspaces else "fail", "bun_lock_workspace_membership",
                          f"Workspace member {member} must be recorded in bun.lock.", [relative, manifestpath, lockpath],
                          "Regenerate the root bun.lock using the approved Bun version after changing workspace members.")
            except ValueError:
                unknown("bun_workspace_patterns_unknown", [relative, lockpath])
            for member, recorded in workspaces.items():
                manifestpath = location(root / Path(relative).parent / member / "package.json")
                if manifestpath not in manifests or not isinstance(recorded, dict):
                    unknown("bun_workspace_unknown", [lockpath])
                    continue
                for section in JS_SECTIONS:
                    declared = manifests[manifestpath].get(section, {})
                    old = recorded.get(section, {})
                    if not isinstance(declared, dict) or not isinstance(old, dict):
                        unknown("bun_workspace_shape_unknown", [manifestpath, lockpath])
                        continue
                    for dep in sorted(set(declared) | set(old)):
                        if not first_party(dep):
                            continue
                        spec = declared.get(dep)
                        check("pass" if spec == old.get(dep) else "fail", "bun_lock_declared_requirement",
                              f"{dep}: manifest {spec!r} versus lock workspace {old.get(dep)!r}.", [manifestpath, lockpath],
                              "Regenerate the template/package bun.lock from its own package.json using the approved Bun version.")
                        if spec is None or isinstance(spec, str) and spec.startswith(("workspace:", "file:", "link:", "git", "github:")):
                            continue
                        versions = []
                        ambiguous = member != "" and any(key != dep and key.endswith("/" + dep) for key in packages)
                        value = packages.get(dep)
                        if ambiguous:
                            versions.append(None)
                        elif value is not None:
                            if not isinstance(value, list) or not value or not isinstance(value[0], str) or not value[0].startswith(dep + "@"):
                                versions.append(None)
                            else:
                                version = value[0][len(dep) + 1:]
                                if version.startswith("workspace:"):
                                    resolved = workspaces.get(version[10:], {})
                                    version = resolved.get("version") if isinstance(resolved, dict) else None
                                versions.append(match(version, spec, "npm"))
                        status = "unknown" if match("0.0.0", spec, "npm") is None else "pass" if True in versions else "unknown" if None in versions else "fail"
                        check(status, "bun_lock_resolved_requirement", f"{dep}: this importer's locked package must satisfy {spec}.", [manifestpath, lockpath],
                              "Regenerate and review the adjacent Bun lock; review importer resolution for ambiguous nested package entries.")
    return inventory, requirements, checks
