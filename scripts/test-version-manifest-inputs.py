"""Offline tests of tracked package/version/lock input collection."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


module = load("manifest_inputs", "version_manifest_inputs.py")
sdk = load("sdk_prerequisites", "check-tool-starter-prerequisites.py")


class ManifestTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)

    def write(self, path, contents, tracked=True):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(contents) if isinstance(contents, dict) else contents)
        if tracked:
            subprocess.run(["git", "-C", str(self.root), "add", "--", path], check=True)

    def scan(self):
        return module.scan_repository(self.root, "test", sdk.compatible)

    def statuses(self, code):
        return [row["status"] for row in self.scan()[2] if row["code"] == code]

    def test_cargo_old_lock_fails_but_matching_git_and_registry_entries_can_coexist(self):
        self.write("Cargo.toml", '[package]\nname="test"\nversion="0.1.0"\n[dependencies]\nlenso-core="=2.0.0"\n')
        lock = 'version=3\n[[package]]\nname="lenso-core"\nversion="1.0.0"\nsource="registry+https://github.com/rust-lang/crates.io-index"\n'
        self.write("Cargo.lock", lock)
        self.assertEqual(self.statuses("cargo_lock_requirement"), ["fail"])
        self.write("Cargo.lock", lock + '[[package]]\nname="lenso-core"\nversion="2.0.0"\nsource="git+https://example.test/core#123"\n')
        self.assertEqual(self.statuses("cargo_lock_requirement"), ["pass"])
        self.assertEqual(self.scan()[1][0]["requirement"], "=2.0.0")

    def test_package_versions_are_independent_and_inherit_nearest_workspace(self):
        self.write("Cargo.toml", '[workspace]\nmembers=["a", "b"]\n[workspace.package]\nversion="9.0.0"\npublish=false\n[workspace.dependencies]\nlenso-c={path="c",version="2.0.0"}\n')
        self.write("a/Cargo.toml", '[package]\nname="lenso-a"\nversion.workspace=true\npublish.workspace=true\n[dependencies]\nlenso-c.workspace=true\nlenso-b={path="../b",version="1.0.0"}\n')
        self.write("b/Cargo.toml", '[package]\nname="lenso-b"\nversion="1.0.4"\n')
        self.write("c/Cargo.toml", '[package]\nname="lenso-c"\nversion="2.0.1"\n')
        self.write("nested/Cargo.toml", '[workspace]\nmembers=["d"]\n[workspace.package]\nversion="3.0.0"\n')
        self.write("nested/d/Cargo.toml", '[package]\nname="lenso-d"\nversion.workspace=true\n')
        rows, requirements, checks = self.scan()
        self.assertEqual({x["name"]: x["version"] for x in rows}, {"lenso-a": "9.0.0", "lenso-b": "1.0.4", "lenso-c": "2.0.1", "lenso-d": "3.0.0"})
        self.assertFalse(any(x["status"] != "pass" for x in checks), checks)
        self.assertFalse(any(x["owner"] == "lenso-a" for x in requirements))
        self.assertEqual(self.statuses("path_dependency_version"), ["pass"] * 3)

    def test_path_mismatch_is_fail_and_git_without_version_is_not_registry_evidence(self):
        self.write("Cargo.toml", '[package]\nname="app"\nversion="0.1.0"\n[dependencies]\nlenso-a={path="a",version="=1.0.0"}\nlenso-b={git="https://example.test/b",rev="abcd"}\n')
        self.write("a/Cargo.toml", '[package]\nname="lenso-a"\nversion="2.0.0"\npublish=false\n')
        rows, requirements, _ = self.scan()
        self.assertEqual(requirements, [])
        self.assertEqual(self.statuses("path_dependency_version"), ["fail"])
        self.assertEqual({x["source"] for x in rows[0]["dependencies"]}, {"path", "git"})

    def test_bun_template_old_declaration_and_old_package_version_fail_separately(self):
        self.write("template/package.json", {"name": "template", "private": True, "version": "0.0.0", "dependencies": {"@lenso/console": "1.2.3"}})
        lock = {"lockfileVersion": 1, "workspaces": {"": {"name": "template", "version": "99.0.0", "dependencies": {"@lenso/console": "1.2.2"}}}, "packages": {"@lenso/console": ["@lenso/console@1.2.2", "", {}, "sha512-test"]}}
        self.write("template/bun.lock", lock)
        self.assertEqual(self.statuses("bun_lock_declared_requirement"), ["fail"])
        self.assertEqual(self.statuses("bun_lock_resolved_requirement"), ["fail"])
        lock["workspaces"][""]["dependencies"]["@lenso/console"] = "1.2.3"
        lock["packages"]["@lenso/console"][0] = "@lenso/console@1.2.3"
        self.write("template/bun.lock", lock)
        self.assertTrue(all(x["status"] == "pass" for x in self.scan()[2]))

    def test_bun_workspace_child_is_checked_and_resolves_workspace_package(self):
        self.write("package.json", {"name": "root", "private": True, "workspaces": ["packages/*"]})
        self.write("packages/a/package.json", {"name": "@lenso/a", "version": "1.0.0", "dependencies": {"@lenso/b": "2.0.0"}})
        self.write("packages/b/package.json", {"name": "@lenso/b", "version": "2.0.0"})
        self.write("bun.lock", {"workspaces": {"": {"name": "root"}, "packages/a": {"name": "@lenso/a", "version": "1.0.0", "dependencies": {"@lenso/b": "2.0.0"}}, "packages/b": {"name": "@lenso/b", "version": "2.0.0"}}, "packages": {"@lenso/b": ["@lenso/b@workspace:packages/b"]}})
        self.assertEqual(self.statuses("bun_lock_resolved_requirement"), ["pass"])
        self.write("packages/a/package.json", {"name": "@lenso/a", "version": "1.0.0", "dependencies": {"@lenso/b": "2.0.1"}})
        self.assertEqual(self.statuses("bun_lock_declared_requirement"), ["fail"])
        self.assertEqual(self.statuses("bun_lock_resolved_requirement"), ["fail"])

    def test_unsupported_requirement_and_malformed_tracked_inputs_are_unknown(self):
        self.write("Cargo.toml", '[package]\nname="app"\nversion="0.1.0"\n[dependencies]\nlenso-core=">=1.0, <3"\n')
        self.write("Cargo.lock", 'version=3\n[[package]]\nname="lenso-core"\nversion="2.0.0"\n')
        self.write("package.json", '{"name":"@lenso/a","name":"@lenso/b"}')
        self.assertEqual(self.statuses("cargo_lock_requirement"), ["unknown"])
        self.assertEqual(self.statuses("manifest_parse_unknown"), ["unknown"])
        self.write("ignored/package.json", '{broken', tracked=False)
        self.assertEqual(self.statuses("manifest_parse_unknown"), ["unknown"])

    def test_json_trailing_comma_parser_preserves_escaped_strings_and_rejects_comments(self):
        value = 'literal,} and escaped \\" quote'
        text = json.dumps({"value": value})[:-1] + ",}"
        self.assertEqual(module.bun_json(text), {"value": value})
        with self.assertRaises(ValueError):
            module.bun_json('{"value":1,// comment\n}')

    def test_missing_inheritance_and_malformed_tables_fail_closed_as_unknown(self):
        self.write("Cargo.toml", '[workspace]\nmembers=["a"]\n')
        self.write("a/Cargo.toml", '[package]\nname="lenso-a"\nversion.workspace=true\npublish.workspace=true\n')
        self.write("bad/Cargo.toml", 'package=[]\n')
        self.write("package.json", {"name": "root", "private": True})
        self.write("bun.lock", {"workspaces": {}, "packages": {}})
        rows, requirements, checks = self.scan()
        self.assertEqual(requirements, [])
        self.assertFalse(rows[0]["publishable"])
        self.assertTrue(all(x["status"] == "unknown" for x in checks))
        self.assertIn("workspace_package_inheritance_unknown", {x["code"] for x in checks})

    def test_cargo_child_uses_workspace_lock_not_its_obsolete_adjacent_lock(self):
        self.write("Cargo.toml", '[workspace]\nmembers=["crates/*"]\n')
        self.write("crates/a/Cargo.toml", '[package]\nname="lenso-a"\nversion="0.1.0"\n[dependencies]\nlenso-core="=2.0.0"\n')
        lock = 'version=3\n[[package]]\nname="lenso-core"\nversion="1.0.0"\nsource="registry+https://github.com/rust-lang/crates.io-index"\n'
        newer = '[[package]]\nname="lenso-core"\nversion="2.0.0"\nsource="git+https://example.test/core#123"\n'
        self.write("Cargo.lock", lock)
        self.write("crates/a/Cargo.lock", 'version=3\n' + newer)
        rows = [x for x in self.scan()[2] if x["code"] == "cargo_lock_requirement"]
        self.assertEqual([x["status"] for x in rows], ["fail"])
        self.assertIn("Cargo.lock", rows[0]["locations"])
        self.assertNotIn("crates/a/Cargo.lock", rows[0]["locations"])
        self.write("Cargo.lock", lock + newer)
        self.assertEqual(self.statuses("cargo_lock_requirement"), ["pass"])

    def test_cargo_excluded_and_nested_workspaces_do_not_borrow_parent_lock(self):
        self.write("Cargo.toml", '[workspace]\nmembers=["crates/*"]\nexclude=["crates/excluded"]\n')
        self.write("Cargo.lock", 'version=3\n[[package]]\nname="lenso-core"\nversion="1.0.0"\n')
        self.write("crates/excluded/Cargo.toml", '[package]\nname="lenso-excluded"\nversion="0.1.0"\n[dependencies]\nlenso-core="=2.0.0"\n')
        self.write("crates/excluded/Cargo.lock", 'version=3\n[[package]]\nname="lenso-core"\nversion="2.0.0"\n')
        self.write("nested/Cargo.toml", '[workspace]\nmembers=["child"]\n')
        self.write("nested/child/Cargo.toml", '[package]\nname="lenso-nested"\nversion="0.1.0"\n[dependencies]\nlenso-core="=3.0.0"\n')
        self.write("nested/Cargo.lock", 'version=3\n[[package]]\nname="lenso-core"\nversion="3.0.0"\n')
        self.write("unlisted/Cargo.toml", '[package]\nname="lenso-unlisted"\nversion="0.1.0"\n[dependencies]\nlenso-core="=9.0.0"\n')
        self.assertEqual(self.statuses("cargo_lock_requirement"), ["pass", "pass"])
        self.assertEqual(self.statuses("cargo_workspace_ownership_unknown"), ["unknown"])

    def test_missing_bun_workspace_member_and_unknown_patterns_do_not_pass(self):
        self.write("package.json", {"name": "root", "private": True, "workspaces": ["packages/*"]})
        self.write("packages/new/package.json", {"name": "@lenso/new", "version": "1.0.0"})
        self.write("bun.lock", {"workspaces": {"": {"name": "root"}}, "packages": {}})
        self.assertEqual(self.statuses("bun_lock_workspace_membership"), ["fail"])
        self.write("package.json", {"name": "root", "private": True, "workspaces": ["packages/**"]})
        self.assertEqual(self.statuses("bun_workspace_patterns_unknown"), ["unknown"])

    def test_bun_root_resolution_cannot_borrow_other_importers_matching_version(self):
        declared = {"name": "root", "private": True, "dependencies": {"@lenso/b": "2.0.0"}}
        self.write("package.json", declared)
        lock = {"workspaces": {"": declared}, "packages": {"@lenso/b": ["@lenso/b@1.0.0"], "other/@lenso/b": ["@lenso/b@2.0.0"]}}
        self.write("bun.lock", lock)
        self.assertEqual(self.statuses("bun_lock_resolved_requirement"), ["fail"])
        lock["packages"]["@lenso/b"] = ["@lenso/b@2.0.0"]
        self.write("bun.lock", lock)
        self.assertEqual(self.statuses("bun_lock_resolved_requirement"), ["pass"])

    def test_ambiguous_bun_child_resolution_is_unknown(self):
        self.write("package.json", {"name": "root", "private": True, "workspaces": ["packages/*"]})
        child = {"name": "@lenso/a", "version": "1.0.0", "dependencies": {"@lenso/b": "2.0.0"}}
        self.write("packages/a/package.json", child)
        self.write("bun.lock", {"workspaces": {"": {"name": "root"}, "packages/a": child},
                                "packages": {"@lenso/b": ["@lenso/b@1.0.0"], "@lenso/a/@lenso/b": ["@lenso/b@2.0.0"]}})
        self.assertEqual(self.statuses("bun_lock_resolved_requirement"), ["unknown"])

    def test_unknown_workspace_globs_are_reported_even_without_child_manifests(self):
        self.write("Cargo.toml", '[workspace]\nmembers=["crates/**"]\n')
        self.write("package.json", {"name": "root", "private": True, "workspaces": ["packages/**"]})
        self.write("bun.lock", {"workspaces": {"": {"name": "root"}}, "packages": {}})
        self.assertEqual(self.statuses("cargo_workspace_ownership_unknown"), ["unknown"])
        self.assertEqual(self.statuses("bun_workspace_patterns_unknown"), ["unknown"])


if __name__ == "__main__":
    unittest.main()
