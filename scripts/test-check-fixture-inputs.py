#!/usr/bin/env python3
"""Reject deliberately broken prepared locks and Tool SDK source cohorts."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("inputs", Path(__file__).with_name("check-fixture-inputs.py"))
inputs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inputs)


class PreparedInputTests(unittest.TestCase):
    def test_missing_and_stale_lock_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("")
            manifest = root / "Cargo.toml"
            manifest.write_text('[package]\nname="agent-lock-negative"\nversion="0.1.0"\nedition="2024"\n[workspace]\n')
            with self.assertRaisesRegex(RuntimeError, "missing prepared fixture lock"):
                inputs.check_locks(root, [manifest])
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root, check=True)
            inputs.check_locks(root, [manifest])
            original = (root / "Cargo.lock").read_bytes()
            manifest.write_text(manifest.read_text().replace('version="0.1.0"', 'version="0.2.0"'))
            with self.assertRaises(subprocess.CalledProcessError):
                inputs.check_locks(root, [manifest])
            self.assertEqual((root / "Cargo.lock").read_bytes(), original)

    def test_sdk_version_drift_is_rejected(self):
        original_root = inputs.release.ROOT
        try:
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                for name in (inputs.release.PROVIDER, inputs.release.MACROS, inputs.release.SDK):
                    path = root / "crates" / name / "Cargo.toml"
                    path.parent.mkdir(parents=True)
                    path.write_bytes((original_root / "crates" / name / "Cargo.toml").read_bytes())
                inputs.release.ROOT = root
                versions = inputs.release.cohort_versions()
                sdk = root / "crates" / inputs.release.SDK / "Cargo.toml"
                sdk.write_text(sdk.read_text().replace(f'version = "{versions[inputs.release.SDK]}"', 'version = "999.0.0"', 1))
                with self.assertRaisesRegex(RuntimeError, "cohort versions do not match"):
                    inputs.release.cohort_versions()
        finally:
            inputs.release.ROOT = original_root


if __name__ == "__main__":
    unittest.main()
