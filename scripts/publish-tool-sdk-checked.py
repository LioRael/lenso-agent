"""Publish the explicitly selected Tool cohort while the reviewed SHA is main."""

from __future__ import annotations

import base64
from collections.abc import Mapping
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
from urllib.request import Request, urlopen


PROVIDER = "lenso-capability-agent-tool-provider"
MACROS = "lenso-agent-tool-sdk-macros"
SDK = "lenso-agent-tool-sdk"
PACKAGE_SETS = {
    "provider-only": (PROVIDER,),
    "sdk-only": (MACROS, SDK),
    "sdk-cohort": (PROVIDER, MACROS, SDK),
}
ROOT = Path(__file__).resolve().parents[1]


def run_git(*arguments: str, environment: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        ["git", *arguments], check=True, capture_output=True, text=True,
        timeout=30, env=environment,
    )
    return result.stdout.strip()


def api_json(api_url: str, path: str, token: str) -> dict:
    request = Request(
        f"{api_url.rstrip('/')}/{path}",
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "Cache-Control": "no-cache",
            "User-Agent": "lenso-tool-sdk-publish-gate/1",
        },
    )
    with urlopen(request, timeout=20) as response:
        payload = response.read(1024 * 1024 + 1)
    if len(payload) > 1024 * 1024:
        raise RuntimeError("GitHub API response is unexpectedly large")
    return json.loads(payload)


def require_pinned_main(
    requested: str, event_sha: str, checkout_sha: str, remote_sha: str,
    api_sha: str, repository: str, repository_id: str, api_repository: dict,
) -> None:
    if not re.fullmatch(r"[0-9a-f]{40}", requested):
        raise RuntimeError("publish revision must be a full commit SHA")
    if any(sha != requested for sha in (event_sha, checkout_sha, remote_sha, api_sha)):
        raise RuntimeError("requested revision, checkout, origin/main, and GitHub API main differ")
    if (
        str(api_repository.get("id")) != repository_id
        or str(api_repository.get("full_name", "")).casefold() != repository.casefold()
        or api_repository.get("default_branch") != "main"
    ):
        raise RuntimeError("GitHub API repository identity differs from the workflow repository")


def check_live_main(environment: Mapping[str, str]) -> None:
    if (
        environment["GITHUB_EVENT_NAME"] != "workflow_dispatch"
        or environment["GITHUB_REF"] != "refs/heads/main"
    ):
        raise RuntimeError("publication requires a main-branch workflow dispatch")
    repository = environment["GITHUB_REPOSITORY"]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise RuntimeError("invalid workflow repository")
    server_url = environment["GITHUB_SERVER_URL"].rstrip("/")
    api_url = environment["GITHUB_API_URL"].rstrip("/")
    if not server_url.startswith("https://") or not api_url.startswith("https://"):
        raise RuntimeError("GitHub endpoints must use HTTPS")
    expected_origin = f"{server_url}/{repository}"
    if run_git("remote", "get-url", "origin") not in (expected_origin, expected_origin + ".git"):
        raise RuntimeError("origin does not identify the workflow repository")

    requested = environment["REQUESTED_REVISION"]
    checkout_sha = run_git("rev-parse", "HEAD")
    if not re.fullmatch(r"[0-9a-f]{40}", requested) or any(
        sha != requested for sha in (environment["GITHUB_SHA"], checkout_sha)
    ):
        raise RuntimeError("requested revision does not match the dispatched checkout")
    token = environment["GH_TOKEN"]
    if not token:
        raise RuntimeError("missing GitHub read token")
    repository_api = api_json(api_url, f"repos/{repository}", token)
    git_environment = dict(environment)
    authorization = base64.b64encode(f"x-access-token:{token}".encode()).decode()
    git_environment.update({
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "http.extraheader",
        "GIT_CONFIG_VALUE_0": f"AUTHORIZATION: basic {authorization}",
        "GIT_TERMINAL_PROMPT": "0",
    })
    remote = run_git("ls-remote", "origin", "refs/heads/main", environment=git_environment)
    fields = remote.split()
    if len(fields) != 2 or fields[1] != "refs/heads/main":
        raise RuntimeError("could not resolve exactly one origin/main ref")
    branch_api = api_json(api_url, f"repos/{repository}/branches/main", token)
    if branch_api.get("name") != "main":
        raise RuntimeError("GitHub API returned a different branch")
    require_pinned_main(
        requested, environment["GITHUB_SHA"], checkout_sha, fields[0],
        branch_api["commit"]["sha"], repository, environment["GITHUB_REPOSITORY_ID"], repository_api,
    )
    print("PASS: checkout, origin/main, and GitHub API main match the reviewed SHA")


def package_version(package: str) -> str:
    manifest = tomllib.loads((ROOT / "crates" / package / "Cargo.toml").read_text())
    return manifest["package"]["version"]


def package_label(package: str) -> str:
    return f"{package} {package_version(package)}"


def report_stop(completed: list[str], next_package: str, attempted: bool) -> None:
    previous = ", ".join(package_label(package) for package in completed) or "none"
    state = "upload may have completed" if attempted else "upload not attempted"
    heading = "PARTIAL PUBLICATION" if completed else (
        "PUBLICATION STATUS UNKNOWN" if attempted else "PUBLICATION STOPPED"
    )
    print(
        f"{heading}: {package_label(next_package)} ({state}); "
        f"prior successful commands: {previous}. Inspect exact registry versions "
        "before any manual resume; no automatic retry.",
        file=sys.stderr,
    )


def main() -> None:
    environment = os.environ
    release_set = environment["RELEASE_SET"]
    if release_set not in PACKAGE_SETS:
        raise RuntimeError("unknown Tool release set")
    packages = PACKAGE_SETS[release_set]
    selected = PROVIDER if release_set == "provider-only" else SDK
    if environment["REQUESTED_VERSION"] != package_version(selected):
        raise RuntimeError("requested version differs from the selected source manifest")
    cargo_environment = {key: value for key, value in environment.items() if key != "GH_TOKEN"}
    if release_set == "sdk-only":
        subprocess.run(
            ["cargo", "info", "--registry", "crates-io", f"{PROVIDER}@{package_version(PROVIDER)}"],
            check=True, env=cargo_environment,
        )
    completed: list[str] = []
    for package in packages:
        try:
            check_live_main(environment)
        except Exception:
            report_stop(completed, package, attempted=False)
            raise
        try:
            subprocess.run(
                ["cargo", "publish", "--locked", "-p", package],
                check=True, env=cargo_environment,
            )
        except Exception:
            report_stop(completed, package, attempted=True)
            raise
        completed.append(package)


if __name__ == "__main__":
    main()
