#!/usr/bin/env python3
"""Pure standard-library regression checks; no documented command is executed."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("entrypoints", Path(__file__).with_name("version_doc_entrypoints.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class EntrypointTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        (self.root / "scripts/check.py").write_text("raise Exception('must not execute')\n")

    def check(self, content, cwd="."):
        (self.root / "README.md").write_text(content)
        return module.check_documents(self.root, [{"path": "README.md", "cwd": cwd}])

    def statuses(self, content, code, **kwargs):
        return [r["status"] for r in self.check(content, **kwargs) if r["code"] == code]

    def test_existing_and_missing_script(self):
        rows = self.check("```sh\npython3 scripts/check.py\nnode scripts/missing.mjs\n```\n")
        self.assertEqual([r["status"] for r in rows[:-1]], ["pass", "fail"])
        self.assertEqual(rows[1]["code"], "script_entrypoint_missing")
        self.assertEqual(rows[1]["locations"], ["README.md:3"])

    def test_literal_cd_and_initial_cwd(self):
        self.assertEqual(self.statuses("```bash\ncd scripts\npython check.py\n```", "script_entrypoint_exists"), ["pass"])
        self.assertEqual(self.statuses("```sh\npython check.py\n```", "script_entrypoint_exists", cwd="scripts"), ["pass"])

    def test_missing_npm_script_and_bun_script(self):
        (self.root / "package.json").write_text(json.dumps({"scripts": {"check": "do-not-run"}}))
        rows = self.check("```shell\nbun run check\nnpm run missing\n```")
        self.assertEqual([r["status"] for r in rows[:-1]], ["pass", "fail"])
        self.assertEqual(rows[1]["code"], "package_script_missing")

    def test_cd_variable_makes_later_cwd_unknown(self):
        rows = self.check("```sh\ncd $DEST\npython3 scripts/check.py\n```")
        self.assertEqual([r["status"] for r in rows[:-1]], ["unknown", "unknown"])

    def test_compound_cd_is_unknown(self):
        rows = self.check("```sh\ncd scripts && python3 check.py\npython3 check.py\n```")
        self.assertEqual([r["status"] for r in rows[:-1]], ["unknown", "unknown"])

    def test_heredoc_body_is_not_interpreted_as_shell(self):
        rows = self.check("```sh\npython3 <<'PY'\nnode missing.mjs\nPY\n```")
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["status"], "unknown")

    def test_fences_reset_cwd_and_ignore_non_shell(self):
        rows = self.check("```sh\ncd scripts\n```\n```sh\npython3 scripts/check.py\n```\n```python\nnode missing.mjs\n```")
        self.assertFalse(any(r["status"] == "fail" for r in rows))
        self.assertEqual(rows[-1]["assessed_count"], 2)

    def test_unassessed_commands_are_counted(self):
        rows = self.check("```sh\ncargo test --locked\nlenso plugin new sample\n```")
        self.assertEqual(rows[-1]["unassessed_count"], 2)
        self.assertEqual(rows[-1]["status"], "unknown")

    def test_continuation_and_comment(self):
        self.assertEqual(self.statuses("```sh\npython3 scripts/check.py \\\n  --offline # comment\n```", "script_entrypoint_exists"), ["pass"])

    def test_module_and_external_path_are_unknown(self):
        rows = self.check("```sh\npython3 -m unittest\nbash /outside/repo.sh\n```")
        self.assertEqual([r["status"] for r in rows[:-1]], ["unknown", "unknown"])

    def test_missing_manifest_and_invalid_manifest(self):
        self.assertEqual(self.statuses("```sh\nnpm run test\n```", "package_manifest_missing"), ["fail"])
        (self.root / "package.json").write_text("invalid")
        self.assertEqual(self.statuses("```sh\nnpm run test\n```", "package_manifest_unreadable"), ["unknown"])

    def test_missing_document_is_actionable(self):
        rows = module.check_documents(self.root, ["missing.md"])
        self.assertEqual(rows[0]["status"], "fail")
        self.assertTrue(rows[0]["remediation"])

    def test_package_selection_flags_are_unknown(self):
        (self.root / "package.json").write_text(json.dumps({"scripts": {"test": "do-not-run"}}))
        rows = self.check("```sh\nnpm run test --prefix missing\nbun run --filter other test\n```")
        self.assertEqual([r["status"] for r in rows[:-1]], ["unknown", "unknown"])

    def test_eof_flushes_unfinished_continuation_after_valid_command(self):
        rows = self.check("```sh\npython3 scripts/check.py\npython3 missing.py " + "\\")
        self.assertEqual(rows[0]["code"], "script_entrypoint_exists")
        incomplete = next(r for r in rows if r["code"] == "shell_ambiguous")
        self.assertEqual(incomplete["status"], "unknown")
        self.assertEqual(incomplete["locations"], ["README.md:3"])
        self.assertFalse(any(r["code"] == "script_entrypoint_missing" for r in rows))
        self.assertEqual(rows[-1]["status"], "unknown")

    def test_unclosed_shell_fence_is_unknown_without_continuation(self):
        rows = self.check("```sh\npython3 scripts/check.py\n")
        incomplete = next(r for r in rows if r["code"] == "shell_fence_unclosed")
        self.assertEqual(incomplete["status"], "unknown")
        self.assertEqual(incomplete["locations"], ["README.md:1"])
        self.assertEqual(rows[-1]["status"], "unknown")

    def test_closed_fence_does_not_hide_unfinished_continuation(self):
        rows = self.check("```sh\npython3 scripts/check.py\npython3 missing.py " + "\\\n```\n")
        self.assertTrue(any(r["code"] == "shell_ambiguous" for r in rows))
        self.assertFalse(any(r["code"] == "shell_fence_unclosed" for r in rows))
        self.assertEqual(rows[-1]["status"], "unknown")

    def test_bun_run_local_file_without_package_manifest(self):
        (self.root / "scripts/check.ts").write_text("throw Error('must not execute');\n")
        rows = self.check("```sh\nbun run scripts/check.ts\n```")
        self.assertEqual(rows[0]["code"], "script_entrypoint_exists")
        self.assertEqual(rows[0]["status"], "pass")

    def test_bun_run_missing_explicit_file_fails(self):
        for entry in ("scripts/missing.ts", "./missing", "missing.ts"):
            with self.subTest(entry=entry):
                rows = self.check(f"```sh\nbun run {entry}\n```")
                self.assertEqual(rows[0]["code"], "script_entrypoint_missing")
                self.assertEqual(rows[0]["status"], "fail")

    def test_bun_run_unknown_bin_is_not_a_missing_package_script(self):
        for package in (None, {"scripts": {}, "bin": {"lenso-console-author": "./bin/author.js"}}):
            with self.subTest(package=package):
                if package is not None:
                    (self.root / "package.json").write_text(json.dumps(package))
                rows = self.check("```sh\nbun run lenso-console-author build\n```")
                self.assertEqual(rows[0]["code"], "bun_entrypoint_unknown")
                self.assertEqual(rows[0]["status"], "unknown")

    def test_bun_package_script_precedes_file_lookup(self):
        (self.root / "package.json").write_text(json.dumps({"scripts": {"scripts/missing.ts": "do-not-run"}}))
        rows = self.check("```sh\nbun run scripts/missing.ts\n```")
        self.assertEqual(rows[0]["code"], "package_script_exists")
        self.assertEqual(rows[0]["status"], "pass")


if __name__ == "__main__":
    unittest.main()
