"""Offline regressions for source identities and unified prerequisite reporting."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("consistency", Path(__file__).with_name("check-version-consistency.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class ConsistencyTests(unittest.TestCase):
    def manifest(self):
        return {"schema": "lenso.cli.npm-candidate.v1", "package": "@lenso/cli", "version": "0.7.1",
                "js_source_sha": "a" * 40, "rust_source_sha": "b" * 40}

    def test_same_version_different_source_hash_is_rejected(self):
        manifest = self.manifest()
        manifest["js_source_sha"] = "c" * 40
        result = gate.check_cli_context(manifest, "a" * 40, "b" * 40, "0.7.1")
        self.assertEqual(result["status"], "fail")
        self.assertIn("js_source_sha", result["message"])

    def test_js_core_pair_mismatch_is_rejected_without_lockstep_versions(self):
        manifest = self.manifest()
        self.assertEqual(gate.check_cli_context(manifest, "a" * 40, "b" * 40, "0.7.1")["status"], "pass")
        result = gate.check_cli_context(manifest, "a" * 40, "d" * 40, "0.7.1")
        self.assertEqual(result["status"], "fail")
        self.assertIn("rust_source_sha", result["message"])

    def test_unavailable_candidate_remains_pending(self):
        selection = {"repositories": {}, "pending_candidates": [
            {"repository": "core", "reference": "055ea6c3", "reason": "full visible ref unavailable"}]}
        roots, sources, checks = gate.selected_sources(selection)
        self.assertEqual(roots, {})
        self.assertEqual(sources, {})
        self.assertEqual(gate.overall(checks), 2)
        self.assertTrue(any(c["code"] == "candidate_pending" for c in checks))

    def test_wrong_checkout_is_not_used_even_with_equal_package_versions(self):
        selection = {"repositories": {"core": {"path": "/tmp/unused", "revision": "a" * 40}}}
        with patch.object(gate, "git", side_effect=["b" * 40, "https://github.com/LioRael/lenso.git", ""]):
            roots, sources, checks = gate.selected_sources(selection)
        self.assertNotIn("core", roots)
        self.assertEqual(sources["core"]["requested_sha"], "a" * 40)
        self.assertEqual(gate.overall(checks), 1)

    def test_dirty_checkout_is_not_a_frozen_source_pass(self):
        selection = {"repositories": {"core": {"path": "/tmp/unused", "revision": "a" * 40}}}
        with patch.object(gate, "git", side_effect=["a" * 40, "https://github.com/LioRael/lenso.git", " M Cargo.toml"]):
            roots, _, checks = gate.selected_sources(selection)
        self.assertNotIn("core", roots)
        self.assertEqual(gate.overall(checks), 2)

    def test_unpublished_pin_reuses_registry_classification_and_action(self):
        row = {"name": "@lenso/example", "ecosystem": "npm", "requirement": "0.2.0", "locations": ["js:package.json"]}
        body = json.dumps({"name": row["name"], "versions": {"0.1.0": {}}}).encode()
        with patch.object(gate.sdk, "fetch", return_value=({"state": "received", "http_status": 200}, body)):
            evidence = gate.probe_requirements([row], False)
        result = gate.registry_check(evidence[0])
        self.assertEqual(result["code"], "version_absent")
        self.assertEqual(result["status"], "fail")
        self.assertIn("package owner", result["remediation"])

    def test_duplicate_requirements_share_one_fresh_public_response(self):
        row = {"name": "@lenso/example", "ecosystem": "npm", "requirement": "0.1.0"}
        body = json.dumps({"name": row["name"], "versions": {"0.1.0": {}}}).encode()
        with patch.object(gate.sdk, "fetch", return_value=({"state": "received", "http_status": 200}, body)) as fetch:
            result = gate.probe_requirements([
                {**row, "role": "source_fixture", "source_override_verifiers": ["examples/a/verify-foundation.py"]},
                {**row, "role": "source_fixture", "source_override_verifiers": ["examples/b/verify-foundation.py"]},
                {**row, "requirement": "^0.1.0"}], False)
        self.assertEqual(len(result), 2)
        self.assertEqual(fetch.call_count, 1)
        exact = next(v for v in result if v["requirement"] == "0.1.0")
        self.assertEqual(exact["source_override_verifiers"], ["examples/a/verify-foundation.py", "examples/b/verify-foundation.py"])

    def test_offline_and_unrecognized_ranges_never_pass(self):
        row = {"name": "lenso-example", "ecosystem": "cargo", "requirement": "=0.1.0"}
        with patch.object(gate.sdk, "urlopen") as network:
            result = gate.probe_requirements([row], True)
        network.assert_not_called()
        self.assertEqual(gate.registry_check(result[0])["status"], "unknown")
        self.assertEqual(gate.registry_check({**row, "state": "unsupported_requirement"})["status"], "unknown")

    def test_unpublished_source_inventory_is_not_itself_a_broken_consumer_pin(self):
        row = {"name": "lenso-example", "ecosystem": "cargo", "requirement": "=0.2.0",
               "state": "version_absent", "roles": ["source_version"]}
        self.assertEqual(gate.registry_check(row)["status"], "unknown")
        self.assertEqual(gate.registry_check({**row, "roles": ["source_version", "dependency"]})["status"], "fail")
        self.assertEqual(gate.registry_check({**row, "roles": ["source_fixture"]})["status"], "unknown")

    def test_only_explicit_source_fixture_patches_change_requirement_scope(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = root / "examples/source-fixture"
            fixture.mkdir(parents=True)
            (fixture / "README.md").write_text("This is source-local evidence.")
            (fixture / "verify-foundation.py").write_text('paths = {"lenso-example": framework / "crates/example"}\npatch = "[patch.crates-io]"\n')
            row = {"name": "lenso-example", "requirement": "0.1.0", "locations": ["examples/source-fixture/app/Cargo.toml"]}
            result = gate.source_fixture_requirements(root, [row, {**row, "name": "lenso-unpatched"}])
            self.assertEqual(result[0]["role"], "source_fixture")
            self.assertNotIn("role", result[1])

    def test_missing_cli_artifact_is_not_source_pair_qualification(self):
        checks = gate.cli_artifact({}, {}, {})
        self.assertEqual(gate.overall(checks), 2)
        self.assertEqual(checks[0]["code"], "artifact_not_supplied")

    def test_lifecycle_source_patches_are_scoped_without_running_verifier(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            example = root / "examples/agent-foundation-lifecycle"
            (example / "runner").mkdir(parents=True)
            (example / "README.md").write_text("source-local public API fixture")
            source = Path(__file__).parents[1] / "examples/agent-foundation-lifecycle/verify.py"
            (example / "verify.py").write_text(source.read_text())
            (example / "runner/Cargo.toml.in").write_text('[dependencies]\nlenso-capability-agent = "0.1.0"\nlenso-unpatched = "0.1.0"\n')
            crate = root / "crates/lenso-capability-agent"
            crate.mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "lenso-capability-agent"\n')
            row = {"name": "lenso-capability-agent", "ecosystem": "cargo", "requirement": "0.1.0", "locations": ["examples/agent-foundation-lifecycle/app/caller/Cargo.toml"]}
            patched, ordinary, unpatched = gate.source_fixture_requirements(root, [row, {**row, "locations": ["apps/consumer/Cargo.toml"]}, {**row, "name": "lenso-unpatched"}])
            self.assertEqual(patched["role"], "source_fixture")
            self.assertNotIn("role", ordinary)
            self.assertNotIn("role", unpatched)
            with patch.object(gate.sdk, "fetch", return_value=({"state": "package_not_found", "http_status": 404}, None)):
                fixture_only = gate.probe_requirements([patched], False)
                evidence = gate.probe_requirements([patched, ordinary], False)
            self.assertEqual(gate.registry_check(fixture_only[0])["status"], "unknown")
            self.assertIn("dependency", evidence[0]["roles"])
            self.assertEqual(gate.registry_check(evidence[0])["status"], "fail")
            (example / "verify.py").write_text(source.read_text().replace('paths[name] = agent / "crates" / name', 'paths[name] = agent / "different" / name'))
            with self.assertRaises(ValueError):
                gate.source_fixture_requirements(root, [row])
            (example / "verify.py").write_text(source.read_text().replace('template.replace("# LENSO_LOCAL_PATCHES", patch)', 'template'))
            with self.assertRaises(ValueError):
                gate.source_fixture_requirements(root, [row])
    def test_owner_failures_do_not_echo_private_child_output(self):
        completed = type("Result", (), {"returncode": 1, "stdout": "", "stderr": "private credential"})()
        with patch.object(gate.subprocess, "run", return_value=completed):
            result = gate.run_check("fixture", ["python3", "unused"], Path("."), ["scripts/unused"], "Run the owner checker.")
        self.assertEqual(result["status"], "fail")
        self.assertNotIn("private", json.dumps(result))
        self.assertIn("output_sha256", result)

    def test_console_private_template_version_is_rewritten_not_forced_equal(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "apps/shell").mkdir(parents=True)
            (root / "apps/shell/package.json").write_text('{"version":"1.20.0"}')
            path = root / "tooling/distribution"
            path.mkdir(parents=True)
            for script in ("package-console.mjs", "package-agent.mjs"):
                (path / script).write_text('apps/shell/package.json\nmanifest.version = version;\nmanifest.optionalDependencies = Object.fromEntries(')
            assets = {f"{exe}-v0.1.13-{target}.tar.gz": "a" * 64 for exe in
                      ("lenso-agent", "lenso-agent-cli", "lenso-agent-acp", "lenso-agent-web", "lenso-agent-console-web")
                      for target in ("darwin-aarch64", "linux-x86_64")}
            (path / "agent-release.json").write_text(json.dumps({"version": "0.1.13", "repository": "LioRael/lenso-agent", "assets": assets}))
            with patch.object(gate.sdk, "urlopen") as network:
                requirements, checks, _ = gate.console_distribution(root, True)
            network.assert_not_called()
            self.assertEqual(len(requirements), 9)
            self.assertTrue(all(v["requirement"] == "=1.20.0" for v in requirements))
            self.assertTrue(all(v["role"] == "source_version" for v in requirements))
            self.assertEqual(checks[0]["status"], "pass")
            self.assertEqual(gate.overall(checks), 2)
            metadata = json.dumps({"tag_name": "v0.1.13", "assets": [{"name": name} for name in assets]}).encode()
            with patch.object(gate.sdk, "fetch", return_value=({"state": "received", "http_status": 200}, metadata)):
                _, metadata_checks, _ = gate.console_distribution(root, False)
            self.assertEqual(gate.overall(metadata_checks), 0)


if __name__ == "__main__":
    unittest.main()
