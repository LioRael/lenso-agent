"""Offline wrapper regressions using tiny local Git repositories and a fake verifier."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("lifecycle_gate", Path(__file__).with_name("check-agent-foundation-lifecycle.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class LifecycleGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="agent-lifecycle-wrapper-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.core, self.agent = self.root / "core", self.root / "agent"
        for root in (self.core, self.agent):
            root.mkdir()
            subprocess.run(["git", "init", "-q", str(root)], check=True)
        (self.core / "tracked").write_text("original\n")
        self.sha = self.commit(self.core)
        self.manifest()
        fixture = self.agent / "examples/agent-foundation-lifecycle/verify.py"
        fixture.parent.mkdir(parents=True)
        fixture.write_text(
            'CORE_PACKAGES=("lenso", "lenso-kernel")\nimport argparse, os\nfrom pathlib import Path\n'
            'p=argparse.ArgumentParser();p.add_argument("--core-root",type=Path);'
            'p.add_argument("--agent-root",type=Path);p.add_argument("--work-dir",type=Path);'
            'p.add_argument("--offline",action="store_true");a=p.parse_args()\n'
            'assert not a.work_dir.exists()\n'
            'assert all(a.work_dir != r and r not in a.work_dir.parents for r in (a.core_root,a.agent_root))\n'
            'a.work_dir.mkdir();(a.work_dir/"fake-verifier-ran").write_text("synthetic only")\n'
            'raise SystemExit(int(os.environ.get("SYNTHETIC_VERIFIER_EXIT","0")))\n'
        )
        self.agent_sha = self.commit(self.agent)

    @staticmethod
    def commit(root):
        subprocess.run(["git", "-C", str(root), "add", "."], check=True)
        subprocess.run(["git", "-C", str(root), "-c", "user.name=fixture", "-c",
                        "user.email=fixture@example.invalid", "commit", "-qm", "synthetic input"], check=True)
        return gate.git(root, "rev-parse", "HEAD")

    def manifest(self, other=None):
        (self.agent / "Cargo.toml").write_text(
            '[patch.crates-io]\n' + "\n".join(
                f'{name}={{git="{gate.CORE_REPOSITORY}",rev="{revision}"}}'
                for name, revision in (("lenso", self.sha), ("lenso-kernel", other or self.sha)))
        )

    def invoke(self, evidence=None, *, code=0, core=None, fetch=False, github_sha=None):
        evidence = evidence or self.root / "proof"
        args = ["wrapper", "--agent-root", str(self.agent), "--evidence-root", str(evidence)]
        if not fetch:
            args += ["--core-root", str(core or self.core), "--offline"]
        environment = {key: value for key, value in os.environ.items() if key != "GITHUB_SHA"}
        environment["SYNTHETIC_VERIFIER_EXIT"] = str(code)
        if github_sha is not None:
            environment["GITHUB_SHA"] = github_sha
        errors = io.StringIO()
        with patch.object(sys, "argv", args), patch.dict(os.environ, environment, clear=True), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(errors):
            result = gate.main()
        return result, errors.getvalue()

    def test_all_core_pins_must_agree_and_be_full_sha(self):
        self.assertEqual(gate.core_revision(self.agent), self.sha)
        for revision in ("0" * 40, "main", self.sha[:8]):
            self.manifest(revision)
            with self.assertRaises(ValueError):
                gate.core_revision(self.agent)
        self.manifest()
        path = self.agent / "Cargo.toml"
        path.write_text(path.read_text().replace(self.sha, self.sha[:8]))
        with self.assertRaisesRegex(ValueError, "full 40-character SHA"):
            gate.core_revision(self.agent)

    def test_wrong_core_head_is_rejected_before_verifier(self):
        with patch.object(gate, "core_revision", return_value="0" * 40):
            result, error = self.invoke()
        self.assertEqual(result, 2)
        self.assertIn("does not match Agent pin", error)
        self.assertFalse((self.root / "proof/consumer").exists())

    def test_required_fixture_core_packages_cannot_use_other_sources_or_be_omitted(self):
        manifest = self.agent / "Cargo.toml"
        original = manifest.read_text()
        for bad in (original.replace(f'lenso-kernel={{git="{gate.CORE_REPOSITORY}"',
                                     'lenso-kernel={git="https://example.invalid/other"'),
                    original.split("lenso-kernel=", 1)[0]):
            manifest.write_text(bad)
            with self.assertRaisesRegex(ValueError, "explicit official Core Git patch"):
                gate.core_revision(self.agent)

    def test_ci_event_sha_must_match_clean_agent_head(self):
        for sha in ("0" * 40, self.agent_sha[:8], ""):
            result, error = self.invoke(github_sha=sha)
            self.assertEqual(result, 2)
            self.assertIn("GITHUB_SHA", error)
            self.assertFalse((self.root / "proof").exists())
        self.assertEqual(self.invoke(github_sha=self.agent_sha)[0], 0)

    def test_dirty_agent_and_core_are_rejected_before_verifier(self):
        for root, label in ((self.agent, "Agent"), (self.core, "Core")):
            dirty = root / "untracked-input"
            dirty.write_text("unreviewed")
            result, error = self.invoke(self.root / label)
            self.assertEqual(result, 2)
            self.assertIn(label + " checkout is dirty", error)
            self.assertFalse((self.root / label / "consumer").exists())
            dirty.unlink()

    def test_source_subdirectory_is_not_a_core_checkout(self):
        subdirectory = self.core / "subdirectory"
        subdirectory.mkdir()
        result, error = self.invoke(core=subdirectory)
        self.assertEqual(result, 2)
        self.assertIn("Git checkout root", error)

    def test_evidence_must_be_outside_both_sources(self):
        for source in (self.core, self.agent):
            evidence = source / "forbidden"
            result, error = self.invoke(evidence)
            self.assertEqual(result, 2)
            self.assertIn("outside both", error)
            self.assertFalse(evidence.exists())

    def test_reused_directory_is_rejected_without_overwriting_receipt(self):
        evidence = self.root / "proof"
        evidence.mkdir()
        receipt = evidence / "lifecycle-gate.json"
        receipt.write_text("keep this proof")
        self.assertEqual(self.invoke(evidence)[0], 2)
        self.assertEqual(receipt.read_text(), "keep this proof")

    def test_fake_verifier_success_and_failure_keep_source_identity_and_exit(self):
        for code in (0, 17):
            evidence = self.root / f"result-{code}"
            self.assertEqual(self.invoke(evidence, code=code)[0], code)
            receipt = json.loads((evidence / "lifecycle-gate.json").read_text())
            self.assertEqual(receipt["core_sha"], self.sha)
            self.assertEqual(receipt["agent_sha"], self.agent_sha)
            self.assertEqual(receipt["verifier_exit_code"], code)
            self.assertEqual(receipt["status"], "passed" if code == 0 else "failed")

    def test_fresh_fetch_uses_exact_public_sha_and_detached_checkout(self):
        commands = []
        def run(command, **_kwargs):
            commands.append(command)
            return subprocess.CompletedProcess(command, 0)
        with patch.object(gate, "checked_checkout", return_value=self.agent_sha), \
                patch.object(gate, "checked_core", return_value=self.sha), \
                patch.object(gate.subprocess, "run", side_effect=run):
            self.assertEqual(self.invoke(fetch=True)[0], 0)
        fetch = next(command for command in commands if "fetch" in command)
        self.assertEqual(fetch[-2:], [gate.CORE_REPOSITORY, self.sha])
        self.assertIn("--depth=1", fetch)
        self.assertIn("--no-tags", fetch)
        self.assertTrue(any("--detach" in command for command in commands))


if __name__ == "__main__":
    unittest.main()
