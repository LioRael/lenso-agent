"""Read-only cross-repository prerequisite gate; never install or publish packages."""
from __future__ import annotations

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import hashlib
from http.client import HTTPException
import io
import json
from pathlib import Path
import re
import socket
import ssl
import subprocess
import tarfile
import tempfile
import tomllib
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlparse
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
SDK = "lenso-agent-tool-sdk"
MACROS = SDK + "-macros"
PROVIDER = "lenso-capability-agent-tool-provider"
NPM_SDK = "@lenso/agent-tool-sdk"
MAX_BYTES = 16 * 1024 * 1024


def toml(path):
    return tomllib.loads(path.read_text())


def template_requirement(source, package):
    pattern = (rf'"{re.escape(package)}"\s*:\s*"([^"\n]+)"' if package.startswith("@")
               else rf'(?m)^{re.escape(package)}\s*=\s*"([^"\n]+)"')
    values = re.findall(pattern, source)
    if len(values) != 1:
        raise ValueError(f"template must declare exactly one {package} requirement")
    value = values[0]
    if re.fullmatch(r"\{[A-Z_]+\}", value):
        matches = re.findall(rf'const {value[1:-1]}: &str = "([^"]+)";', source)
        if len(matches) != 1:
            raise ValueError(f"cannot resolve template constant for {package}")
        value = matches[0]
    if not re.fullmatch(r"\d+\.\d+\.\d+", value):
        raise ValueError(f"unsupported template requirement for {package}")
    return value


def compatible(version, requirement, ecosystem):
    # Deliberately bounded to the exact/caret/tilde forms in this cohort.
    match = re.fullmatch(r"([=^~]?)(\d+)\.(\d+)\.(\d+)", requirement)
    if not match:
        raise ValueError(f"unsupported {ecosystem} requirement: {requirement}")
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        return False
    kind = match[1] or ("^" if ecosystem == "cargo" else "=")
    base = tuple(map(int, match.groups()[1:]))
    candidate = tuple(map(int, version.split(".")))
    if kind == "=":
        return candidate == base
    if kind == "~":
        upper = (base[0], base[1] + 1, 0)
    elif base[0]:
        upper = (base[0] + 1, 0, 0)
    elif base[1]:
        upper = (0, base[1] + 1, 0)
    else:
        upper = (0, 0, base[2] + 1)
    return base <= candidate < upper


def fetch(url, offline=False):
    result = {"url": url, "observed_utc": datetime.now(timezone.utc).isoformat()}
    if offline:
        return {**result, "state": "offline_unchecked"}, None
    try:
        with urlopen(Request(url, headers={"User-Agent": "lenso-starter-prerequisites/1", "Cache-Control": "no-cache"}), timeout=20) as response:
            body = response.read(MAX_BYTES + 1)
            result.update(http_status=response.status, etag=response.headers.get("ETag"))
    except HTTPError as error:
        state = ("package_not_found" if error.code == 404 else
                 "access_error" if error.code in (401, 403) else
                 "rate_limited" if error.code == 429 else "registry_error")
        return {**result, "http_status": error.code, "state": state}, None
    except (URLError, TimeoutError, OSError, HTTPException) as error:
        reason = getattr(error, "reason", error)
        state = ("tls_error" if isinstance(reason, ssl.SSLError) else
                 "dns_error" if isinstance(reason, socket.gaierror) else
                 "timeout" if isinstance(reason, TimeoutError) else "network_error")
        # Do not echo proxy URLs, authentication headers or arbitrary exception text.
        return {**result, "state": state}, None
    if len(body) > MAX_BYTES or result["http_status"] != 200:
        return {**result, "state": "invalid_response"}, None
    return {**result, "state": "received", "body_sha256": hashlib.sha256(body).hexdigest()}, body


def registry_probe(item, offline=False):
    name, ecosystem = item["name"], item["ecosystem"]
    try:
        compatible("0.0.0", item["requirement"], ecosystem)
    except ValueError:
        return {**item, "state": "unsupported_requirement"}
    if ecosystem == "cargo":
        lower = name.lower()
        prefix = lower[:2] + "/" + lower[2:4]  # All selected names are >= 4 chars.
        url = f"https://index.crates.io/{prefix}/{lower}"
    else:
        url = "https://registry.npmjs.org/" + quote(name, safe="@")
    evidence, body = fetch(url, offline)
    result = {**item, **evidence}
    if body is None:
        return result
    try:
        if ecosystem == "cargo":
            versions = {v["vers"]: v for v in map(json.loads, body.splitlines())}
            if not versions or any(v["name"] != name for v in versions.values()):
                raise ValueError("identity mismatch")
            usable = [v for v in versions if not versions[v]["yanked"]]
        else:
            metadata = json.loads(body)
            if metadata["name"] != name:
                raise ValueError("identity mismatch")
            versions = metadata["versions"]
            usable = list(versions)
        matches = [v for v in usable if compatible(v, item["requirement"], ecosystem)]
        occupied_matches = [v for v in versions if compatible(v, item["requirement"], ecosystem)]
        result.update(state="metadata_available" if matches else
                      "version_yanked" if occupied_matches else "version_absent",
                      matching_versions=matches, observed_versions=list(versions),
                      usable_versions=usable)
        if item.get("archive_identity") and matches:
            version = item["requirement"].removeprefix("=")
            result["archive"] = verify_published(item, versions[version])
    except (KeyError, TypeError, ValueError, json.JSONDecodeError, tarfile.TarError):
        result["state"] = "invalid_metadata_or_archive"
    return result


def verify_published(item, metadata):
    name, version = item["name"], item["requirement"].removeprefix("=")
    cargo = item["ecosystem"] == "cargo"
    url = (f"https://static.crates.io/crates/{name}/{name}-{version}.crate" if cargo
           else metadata["dist"]["tarball"])
    parsed = urlparse(url)
    expected_host = "static.crates.io" if cargo else "registry.npmjs.org"
    if parsed.scheme != "https" or parsed.hostname != expected_host or parsed.username or parsed.password:
        raise ValueError("unexpected public archive origin")
    evidence, body = fetch(url)
    if body is None:
        return evidence
    if cargo:
        valid = hashlib.sha256(body).hexdigest() == metadata["cksum"]
    else:
        valid = any(sri == "sha512-" + base64.b64encode(hashlib.sha512(body).digest()).decode()
                    for sri in metadata["dist"]["integrity"].split())
    if not valid:
        return {**evidence, "state": "integrity_mismatch"}
    with tarfile.open(fileobj=io.BytesIO(body), mode="r:gz") as archive:
        prefix = f"{name}-{version}" if cargo else "package"
        manifest = archive.extractfile(prefix + ("/Cargo.toml" if cargo else "/package.json"))
        if manifest is None:
            raise ValueError("missing archive manifest")
        data = tomllib.loads(manifest.read().decode()) if cargo else json.load(manifest)
        identity = data["package"] if cargo else data
        if (identity["name"], identity["version"]) != (name, version):
            raise ValueError("archive identity mismatch")
        for key, expected in item.get("archive_fields", {}).items():
            if data.get(key) != expected:
                raise ValueError("published API metadata differs from reviewed source")
        for dependency, expected in item.get("archive_dependencies", {}).items():
            spec = data["dependencies"][dependency]
            if spec.get("version") != expected or any(k in spec for k in ("path", "git", "registry")):
                raise ValueError("published Rust SDK is outside the selected registry cohort")
        for relative, expected in item.get("archive_json", {}).items():
            member = archive.getmember(prefix + "/" + relative)
            if member.size > MAX_BYTES or json.load(archive.extractfile(member)) != expected:
                raise ValueError("published wire contract differs from reviewed source")
        if not cargo:
            for export in data["exports"].values():
                for target in export.values():
                    archive.getmember("package/" + target.removeprefix("./"))
    return {**evidence, "state": "archive_verified", "sha256": hashlib.sha256(body).hexdigest()}


def read_source(agent, lenso):
    manifests = {name: toml(agent / "crates" / name / "Cargo.toml") for name in (PROVIDER, MACROS, SDK)}
    npm = json.loads((agent / "packages/lenso-agent-tool-sdk/package.json").read_text())
    source = (lenso / "crates/lenso-engine-app/src/plugin/scaffold.rs").read_text()
    errors = []
    for name, version in ((SDK, manifests[SDK]["package"]["version"]), (NPM_SDK, npm["version"])):
        if template_requirement(source, name) != version:
            errors.append(f"template/source version mismatch: {name}")
    for name in (PROVIDER, MACROS):
        if manifests[SDK]["dependencies"][name]["version"] != manifests[name]["package"]["version"]:
            errors.append(f"SDK dependency/source version mismatch: {name}")
    if manifests[SDK]["package"]["version"] != manifests[MACROS]["package"]["version"]:
        errors.append("SDK and macro versions differ")
    if any(v["package"].get("publish") is not True for v in manifests.values()) or npm.get("private"):
        errors.append("selected SDK cohort is not publishable")
    if template_requirement(source, "@lenso/bun-plugin") != npm["dependencies"]["@lenso/bun-plugin"]:
        errors.append("Bun Plugin SDK requirement differs between template and Agent SDK")
    facade = re.findall(r'package = "lenso-plugin-sdk", version = "([^"]+)"', source)
    facade_manifest = toml(lenso / "crates/lenso-plugin-sdk/Cargo.toml")
    if facade != [facade_manifest["package"]["version"]]:
        errors.append("portable Rust Plugin SDK template/source version mismatch")
    return manifests, npm, errors


def check_rust_authoring_exports(rust):
    # Bounded to this SDK's explicit re-export layout, not a general Rust parser.
    # Changed layouts require review; token presence alone is not public visibility.
    rust = re.sub(r"//[^\n]*|/\*.*?\*/", "", rust, flags=re.S)
    macro = re.search(r"(?m)^pub\s+use\s+lenso_agent_tool_sdk_macros::tool_provider\s*;", rust)
    prelude = re.search(
        r"(?m)^pub\s+mod\s+prelude\s*\{\s*"
        r"pub\s+use\s+crate::tool_provider\s*;\s*"
        r"pub\s+use\s+lenso_capability_agent_tool_provider\s+as\s+tool_provider_contract\s*;\s*"
        r"pub\s+use\s+lenso_capability_agent_tool_provider::\{([^{}]+)\}\s*;\s*\}", rust)
    symbols = {v.strip() for v in prelude[1].split(",")} if prelude else set()
    if not macro or not {"ContentType", "ExecuteError", "ExecuteResponse"}.issubset(symbols):
        raise ValueError("Rust starter public re-export layout is absent or changed")


def api_evidence(agent, npm, codegen):
    contract = agent / "crates" / PROVIDER
    descriptor = json.loads((contract / "capability.json").read_text())
    with tempfile.TemporaryDirectory(prefix="lenso-starter-contract-") as directory:
        output = Path(directory) / "contract.ts"
        completed = subprocess.run([str(codegen), "generate", str(contract / "capability.json"), "--typescript", str(output)],
                                   capture_output=True, timeout=60)
        if completed.returncode:
            raise ValueError("selected codegen could not generate the committed contract")
        match = re.search(r'DESCRIPTOR_DIGEST[^=]*=\s*"(sha256:[0-9a-f]{64})"', output.read_text())
        if not match:
            raise ValueError("generated contract has no digest")
    expected = {"capability_id": descriptor["id"], "descriptor_version": descriptor["version"],
                "descriptor_digest": match[1], "request_operations": [v["name"] for v in descriptor["operations"]]}
    if any(v["interaction"] != "request" for v in descriptor["operations"]):
        raise ValueError("Tool starter requires a Request contract")
    build = npm["lenso"]["build"]
    if build["provider_contracts"] != [expected] or build["api_version"] != 1 or build["export"] != "./lenso-build":
        raise ValueError("npm build API metadata differs from generated contract")
    for relative in (f"crates/{PROVIDER}/src/generated.rs", "packages/lenso-agent-tool-sdk/src/index.ts", "packages/lenso-agent-tool-sdk/src/lenso-build.ts"):
        if expected["descriptor_digest"] not in (agent / relative).read_text():
            raise ValueError(f"contract digest differs: {relative}")
    if not {"tools", "tool"}.issubset(build["declarations"]["."]) or "./schema" not in npm["exports"]:
        raise ValueError("Bun starter declarations are absent")
    for subpath, symbols in build["declarations"].items():
        file = "index.ts" if subpath == "." else "schema.ts"
        text = (agent / "packages/lenso-agent-tool-sdk/src" / file).read_text()
        if any(not re.search(rf"export function {symbol}\b", text) for symbol in symbols):
            raise ValueError("declared Bun authoring symbol is absent from source")
    rust = (agent / "crates" / SDK / "src/lib.rs").read_text()
    check_rust_authoring_exports(rust)
    return {**expected, "codegen_sha256": hashlib.sha256(codegen.read_bytes()).hexdigest(),
            "scope": "generated wire contract and declared source exports; not a compilation/ABI proof"}


def package_requirements(agent, lenso, manifests, npm):
    workspace = toml(agent / "Cargo.toml")["workspace"]["dependencies"]
    rows = []
    for name, manifest in manifests.items():
        rows.append({"name": name, "ecosystem": "cargo", "requirement": "=" + manifest["package"]["version"],
                     "owner": "reviewed Tool cohort", "archive_identity": True})
        if name == SDK:
            rows[-1]["archive_dependencies"] = {dep: manifests[dep]["package"]["version"] for dep in (PROVIDER, MACROS)}
        if name == PROVIDER:
            contract = agent / "crates" / PROVIDER
            descriptor = json.loads((contract / "capability.json").read_text())
            paths = {"capability.json"} | {operation[field] for operation in descriptor["operations"]
                    for field in ("request_schema", "response_schema", "domain_error_schema")}
            rows[-1]["archive_json"] = {path: json.loads((contract / path).read_text()) for path in sorted(paths)}
        for section in ("dependencies", "build-dependencies"):
            for dependency, spec in manifest.get(section, {}).items():
                if not dependency.startswith("lenso-") or dependency in manifests:
                    continue
                if isinstance(spec, dict) and spec.get("workspace"):
                    spec = workspace[dependency]
                version = spec if isinstance(spec, str) else spec["version"]
                rows.append({"name": dependency, "ecosystem": "cargo", "requirement": version, "owner": name + ":" + section})
    rows.append({"name": NPM_SDK, "ecosystem": "npm", "requirement": npm["version"],
                 "owner": "reviewed Tool cohort", "archive_identity": True,
                 "archive_fields": {key: npm[key] for key in ("exports", "lenso", "dependencies")}})
    facade = toml(lenso / "crates/lenso-plugin-sdk/Cargo.toml")["package"]
    rows.append({"name": facade["name"], "ecosystem": "cargo", "requirement": "=" + facade["version"],
                 "owner": "portable Rust template", "archive_identity": True})
    for section in ("dependencies", "devDependencies"):
        rows.extend({"name": name, "ecosystem": "npm", "requirement": version, "owner": NPM_SDK + ":" + section}
                    for name, version in npm[section].items())
    # Owner-labelled direct first-party Rust prerequisites, not a Cargo resolver.
    return rows


def exit_status(errors, results):
    bad = {"version_absent", "version_yanked", "package_not_found", "invalid_metadata_or_archive", "integrity_mismatch", "unsupported_requirement"}
    if errors or any(v["state"] in bad or v.get("archive", {}).get("state") in bad for v in results):
        return 1
    if any(v["state"] != "metadata_available" or
           v.get("archive", {}).get("state", "archive_verified") != "archive_verified" for v in results):
        return 2
    return 0


def build_report(agent, lenso, codegen, offline=False):
    """Shared evidence collector used by the broader consistency report."""
    try:
        manifests, npm, errors = read_source(agent, lenso)
    except (ValueError, KeyError, TypeError, OSError) as error:
        return {"schema": "lenso.tool-starter-prerequisites@1", "exit_code": 1,
                "source_errors": [f"invalid or missing source manifest/template ({type(error).__name__})"],
                "registry": []}
    try:
        api = api_evidence(agent, npm, codegen)
    except (ValueError, KeyError, OSError, subprocess.TimeoutExpired) as error:
        errors.append(str(error) if isinstance(error, ValueError) else "source contract check could not complete")
        api = {"state": "unverified"}
    rows = package_requirements(agent, lenso, manifests, npm)
    patches = toml(agent / "Cargo.toml").get("patch", {}).get("crates-io", {})
    checked_names = {row["name"] for row in rows if row["ecosystem"] == "cargo"}
    overrides = {name: {"kind": "git" if "git" in value else "local_or_other",
                        "revision": value.get("rev")}
                 for name, value in patches.items() if name in checked_names}
    with ThreadPoolExecutor(max_workers=4) as pool:
        results = list(pool.map(lambda row: registry_probe(row, offline), rows))
    status = exit_status(errors, results)
    revisions = {name: subprocess.check_output(["git", "-C", str(path), "rev-parse", "HEAD"], text=True).strip()
                 for name, path in (("agent", agent), ("lenso", lenso))}
    dirty = {name: bool(subprocess.check_output(["git", "-C", str(path), "status", "--porcelain"]))
             for name, path in (("agent", agent), ("lenso", lenso))}
    return {"schema": "lenso.tool-starter-prerequisites@1", "source_revisions": revisions,
                      "worktree_dirty": dirty, "workspace_overrides_not_used_as_registry_proof": overrides,
                      "exit_code": status, "source_errors": errors, "source_api": api,
                      "registry": results, "registry_scope": "official public sources; private overrides are not assessed",
            "not_proven": ["registry-only Rust compile", "Bun starter runtime", "CLI release qualification", "publication authorization"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lenso-root", type=Path, required=True)
    parser.add_argument("--codegen", type=Path, required=True)
    parser.add_argument("--offline", action="store_true", help="check local inputs; registry remains explicitly unchecked")
    args = parser.parse_args()
    report = build_report(ROOT, args.lenso_root.resolve(), args.codegen.resolve(), args.offline)
    print(json.dumps(report, indent=2))
    raise SystemExit(report["exit_code"])


if __name__ == "__main__":
    main()
