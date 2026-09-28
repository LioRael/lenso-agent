"""Offline fail-closed checks for the selected Tool release boundary."""

from __future__ import annotations

from contextlib import redirect_stderr, redirect_stdout
import importlib.util
from io import StringIO
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch


sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "publish_tool_sdk_checked", Path(__file__).with_name("publish-tool-sdk-checked.py")
)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

SHA = "a" * 40
OTHER_SHA = "b" * 40
REPOSITORY = "LioRael/lenso-agent"


class PublishGateTests(unittest.TestCase):
    def exercise(
        self, *, remote_heads=(SHA,), api_heads=(SHA,), cargo_failure=None,
        env_changes=None, origin=None, repo_id=123, api_failure=False,
        release_set="provider-only",
    ):
        environment = {
            "GITHUB_EVENT_NAME": "workflow_dispatch",
            "GITHUB_REF": "refs/heads/main",
            "GITHUB_REPOSITORY": REPOSITORY,
            "GITHUB_REPOSITORY_ID": "123",
            "GITHUB_SERVER_URL": "https://github.com",
            "GITHUB_API_URL": "https://api.github.com",
            "GITHUB_SHA": SHA,
            "REQUESTED_REVISION": SHA,
            "RELEASE_SET": release_set,
            "REQUESTED_VERSION": "0.3.0" if release_set == "provider-only" else "0.4.0",
            "GH_TOKEN": "fake-token",
            "CARGO_REGISTRY_TOKEN": "fake-cargo-token",
        }
        environment.update(env_changes or {})
        uploads = []
        git_reads = 0
        api_reads = 0
        repo_reads = 0

        def git(*arguments, environment=None):
            nonlocal git_reads
            if arguments == ("remote", "get-url", "origin"):
                return origin or f"https://github.com/{REPOSITORY}.git"
            if arguments == ("rev-parse", "HEAD"):
                return SHA
            if arguments == ("ls-remote", "origin", "refs/heads/main"):
                index = min(git_reads, len(remote_heads) - 1)
                git_reads += 1
                return f"{remote_heads[index]}\trefs/heads/main"
            raise AssertionError(arguments)

        def api(_url, path, _token):
            nonlocal api_reads, repo_reads
            if api_failure:
                raise RuntimeError("API unavailable")
            if path == f"repos/{REPOSITORY}":
                identities = repo_id if isinstance(repo_id, tuple) else (repo_id,)
                index = min(repo_reads, len(identities) - 1)
                repo_reads += 1
                return {"id": identities[index], "full_name": REPOSITORY, "default_branch": "main"}
            if path == f"repos/{REPOSITORY}/branches/main":
                index = min(api_reads, len(api_heads) - 1)
                api_reads += 1
                return {"name": "main", "commit": {"sha": api_heads[index]}}
            raise AssertionError(path)

        def cargo(command, **_kwargs):
            if command[:2] == ["cargo", "info"]:
                return
            uploads.append(command)
            if cargo_failure == len(uploads):
                raise subprocess.CalledProcessError(1, command)

        output = StringIO()
        with (
            patch.object(gate.os, "environ", environment),
            patch.object(gate, "run_git", git),
            patch.object(gate, "api_json", api),
            patch.object(gate.subprocess, "run", cargo),
            redirect_stdout(output),
            redirect_stderr(output),
        ):
            try:
                gate.main()
            except (RuntimeError, subprocess.CalledProcessError) as error:
                return uploads, output.getvalue(), error
        return uploads, output.getvalue(), None

    def test_only_the_provider_is_checked_and_published(self):
        uploads, _, error = self.exercise()
        self.assertIsNone(error)
        self.assertEqual(
            [command[-1] for command in uploads],
            ["lenso-capability-agent-tool-provider"],
        )
        self.assertTrue(all(command[:4] == ["cargo", "publish", "--locked", "-p"] for command in uploads))

    def test_explicit_sdk_cohort_keeps_dependency_order(self):
        uploads, _, error = self.exercise(release_set="sdk-cohort")
        self.assertIsNone(error)
        self.assertEqual(
            [command[-1] for command in uploads],
            [gate.PROVIDER, gate.MACROS, gate.SDK],
        )

    def test_explicit_sdk_only_does_not_reupload_provider(self):
        uploads, _, error = self.exercise(release_set="sdk-only")
        self.assertIsNone(error)
        self.assertEqual([command[-1] for command in uploads], [gate.MACROS, gate.SDK])

    def test_sdk_cohort_stops_if_main_moves_after_provider(self):
        uploads, output, error = self.exercise(
            release_set="sdk-cohort", remote_heads=(SHA, OTHER_SHA)
        )
        self.assertIsInstance(error, RuntimeError)
        self.assertEqual([command[-1] for command in uploads], [gate.PROVIDER])
        self.assertIn("PARTIAL PUBLICATION", output)
        self.assertIn("lenso-agent-tool-sdk-macros 0.4.0 (upload not attempted)", output)

    def test_initial_mismatches_fail_before_any_upload(self):
        cases = (
            {"env_changes": {"REQUESTED_REVISION": "$(echo unsafe)"}},
            {"env_changes": {"REQUESTED_VERSION": "0.4.0"}},
            {"env_changes": {"RELEASE_SET": "unknown"}},
            {"env_changes": {"GITHUB_SHA": OTHER_SHA}},
            {"origin": "https://github.com/other/repo.git"},
            {"remote_heads": (OTHER_SHA,)},
            {"remote_heads": ("",)},
            {"api_heads": (OTHER_SHA,)},
            {"repo_id": 999},
            {"api_failure": True},
        )
        for case in cases:
            with self.subTest(case=case):
                uploads, _, error = self.exercise(**case)
                self.assertIsNotNone(error)
                self.assertEqual(uploads, [])

    def test_failed_upload_is_reported_as_uncertain_without_retry(self):
        uploads, output, error = self.exercise(cargo_failure=1)
        self.assertIsInstance(error, subprocess.CalledProcessError)
        self.assertEqual(len(uploads), 1)
        self.assertIn("PUBLICATION STATUS UNKNOWN", output)
        self.assertIn("upload may have completed", output)
        self.assertIn("no automatic retry", output)


if __name__ == "__main__":
    unittest.main()
