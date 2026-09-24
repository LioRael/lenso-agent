"""Publish the Tool SDK cohort only while the reviewed SHA is still main."""

from __future__ import annotations

import base64
import json
import os
import re
import subprocess
from urllib.request import Request, urlopen


PACKAGES = (
    "lenso-capability-agent-tool-provider",
    "lenso-agent-tool-sdk-macros",
    "lenso-agent-tool-sdk",
)


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


def main() -> None:
    environment = os.environ
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
    cargo_environment = {key: value for key, value in environment.items() if key != "GH_TOKEN"}
    command = ["cargo", "publish", "--locked"]
    for package in PACKAGES:
        command.extend(("-p", package))
    subprocess.run(command, check=True, env=cargo_environment)


if __name__ == "__main__":
    main()
