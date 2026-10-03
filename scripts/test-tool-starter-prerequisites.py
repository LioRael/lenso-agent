"""Offline regression coverage for the read-only starter prerequisite gate."""
import importlib.util
from http.client import IncompleteRead
import io
import json
from pathlib import Path
import socket
import ssl
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError, URLError

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("prerequisites", Path(__file__).with_name("check-tool-starter-prerequisites.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class Response:
    status = 200
    headers = {"ETag": "test"}

    def __init__(self, value):
        self.body = value

    def read(self, _maximum):
        return self.body

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False


class PrerequisitesTests(unittest.TestCase):
    def test_template_source_drift_is_a_failure_even_before_registry_access(self):
        with tempfile.TemporaryDirectory() as directory:
            agent, lenso = Path(directory) / "agent", Path(directory) / "lenso"
            for relative in [f"crates/{name}/Cargo.toml" for name in (gate.PROVIDER, gate.MACROS, gate.SDK)] + ["packages/lenso-agent-tool-sdk/package.json"]:
                target = agent / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes((gate.ROOT / relative).read_bytes())
            target = lenso / "crates/lenso-engine-app/src/plugin/scaffold.rs"
            target.parent.mkdir(parents=True)
            target.write_text('lenso-agent-tool-sdk = "9.9.9"\n"@lenso/agent-tool-sdk": "0.1.1"\n"@lenso/bun-plugin": "0.4.2"\nlenso = { package = "lenso-plugin-sdk", version = "0.4.6" }')
            facade = lenso / "crates/lenso-plugin-sdk/Cargo.toml"
            facade.parent.mkdir(parents=True)
            facade.write_text('[package]\nname="lenso-plugin-sdk"\nversion="0.4.6"\n')
            _, _, errors = gate.read_source(agent, lenso)
            self.assertIn("template/source version mismatch: lenso-agent-tool-sdk", errors)
            self.assertEqual(gate.exit_status(errors, []), 1)

    def test_template_pin_is_read_from_current_constant_or_literal(self):
        self.assertEqual(gate.template_requirement('lenso-agent-tool-sdk = "0.4.0"', gate.SDK), "0.4.0")
        source = 'const SDK_VERSION: &str = "0.4.1";\nlenso-agent-tool-sdk = "{SDK_VERSION}"'
        self.assertEqual(gate.template_requirement(source, gate.SDK), "0.4.1")
        with self.assertRaises(ValueError):
            gate.template_requirement(source + '\nlenso-agent-tool-sdk = "0.3.3"', gate.SDK)

    def test_missing_version_is_not_replaced_with_latest_or_older_sdk(self):
        body = json.dumps({"name": gate.NPM_SDK, "versions": {"0.1.0": {}}, "dist-tags": {"latest": "0.1.0"}}).encode()
        row = {"name": gate.NPM_SDK, "ecosystem": "npm", "requirement": "0.1.1"}
        with patch.object(gate, "urlopen", return_value=Response(body)):
            result = gate.registry_probe(row)
        self.assertEqual(result["state"], "version_absent")
        self.assertEqual(result["http_status"], 200)
        self.assertEqual(gate.exit_status([], [result]), 1)

    def test_cargo_caret_does_not_admit_previous_minor_or_yanked_release(self):
        self.assertFalse(gate.compatible("0.3.3", "0.4.0", "cargo"))
        self.assertTrue(gate.compatible("0.4.1", "0.4.0", "cargo"))
        self.assertFalse(gate.compatible("0.5.0", "0.4.0", "cargo"))
        row = {"name": gate.SDK, "ecosystem": "cargo", "requirement": "=0.4.0"}
        body = json.dumps({"name": gate.SDK, "vers": "0.4.0", "yanked": True}).encode()
        with patch.object(gate, "urlopen", return_value=Response(body)):
            result = gate.registry_probe(row)
        self.assertEqual(result["state"], "version_yanked")
        self.assertEqual(result["observed_versions"], ["0.4.0"])
        self.assertEqual(result["usable_versions"], [])
        self.assertEqual(result["matching_versions"], [])
        self.assertEqual(gate.exit_status([], [result]), 1)

    def test_cargo_missing_and_yanked_versions_are_distinct(self):
        row = {"name": gate.SDK, "ecosystem": "cargo", "requirement": "=0.4.0"}
        body = json.dumps({"name": gate.SDK, "vers": "0.3.3", "yanked": True}).encode()
        with patch.object(gate, "urlopen", return_value=Response(body)):
            result = gate.registry_probe(row)
        self.assertEqual(result["state"], "version_absent")
        self.assertEqual(result["observed_versions"], ["0.3.3"])
        self.assertEqual(result["usable_versions"], [])

    def test_private_or_commented_rust_imports_do_not_count_as_public_exports(self):
        rust = (gate.ROOT / "crates" / gate.SDK / "src/lib.rs").read_text()
        gate.check_rust_authoring_exports(rust)
        for changed in (rust.replace("pub use ", "use "),
                        rust.replace("pub mod prelude", "mod prelude"),
                        rust.replace("pub use ", "pub(crate) use "),
                        "/*\n" + rust + "\n*/"):
            with self.subTest(source=changed), self.assertRaisesRegex(ValueError, "public re-export"):
                gate.check_rust_authoring_exports(changed)

    def test_truncated_http_body_is_network_unknown_without_exception_text(self):
        response = Response(b"")
        with patch.object(response, "read", side_effect=IncompleteRead(b"private credential", 100)), \
                patch.object(gate, "urlopen", return_value=response):
            result, body = gate.fetch("https://registry.npmjs.org/example")
        self.assertEqual(result["state"], "network_error")
        self.assertIsNone(body)
        self.assertNotIn("private", json.dumps(result))
        self.assertEqual(gate.exit_status([], [result]), 2)

    def test_offline_never_calls_registry_or_reports_availability(self):
        with patch.object(gate, "urlopen") as request:
            result, body = gate.fetch("https://registry.npmjs.org/example", offline=True)
        request.assert_not_called()
        self.assertIsNone(body)
        self.assertEqual(result["state"], "offline_unchecked")
        self.assertEqual(gate.exit_status([], [result]), 2)

    def test_unsupported_requirement_is_not_approximated_or_queried(self):
        with patch.object(gate, "urlopen") as request:
            result = gate.registry_probe({"name": gate.SDK, "ecosystem": "cargo", "requirement": ">=0.4"})
        request.assert_not_called()
        self.assertEqual(result["state"], "unsupported_requirement")
        self.assertEqual(gate.exit_status([], [result]), 1)

    def test_access_rate_limit_and_network_are_unknown_not_missing(self):
        for code, state in [(401, "access_error"), (403, "access_error"), (429, "rate_limited"), (503, "registry_error")]:
            with self.subTest(code=code), patch.object(gate, "urlopen", side_effect=HTTPError("https://registry.npmjs.org/example", code, "error", {}, None)):
                result, _ = gate.fetch("https://registry.npmjs.org/example")
                self.assertEqual(result["state"], state)
                self.assertEqual(gate.exit_status([], [result]), 2)
        for reason, state in [(socket.gaierror("private proxy"), "dns_error"), (ssl.SSLError("private credential"), "tls_error"), (TimeoutError(), "timeout")]:
            with self.subTest(state=state), patch.object(gate, "urlopen", side_effect=URLError(reason)):
                result, _ = gate.fetch("https://registry.npmjs.org/example")
                self.assertEqual(result["state"], state)
                self.assertNotIn("private", json.dumps(result))
                self.assertEqual(gate.exit_status([], [result]), 2)

    def test_404_and_invalid_200_are_distinct(self):
        with patch.object(gate, "urlopen", side_effect=HTTPError("https://registry.npmjs.org/example", 404, "missing", {}, None)):
            result, _ = gate.fetch("https://registry.npmjs.org/example")
            self.assertEqual(result["state"], "package_not_found")
        with patch.object(gate, "urlopen", return_value=Response(b"<html>proxy</html>")):
            result = gate.registry_probe({"name": gate.NPM_SDK, "ecosystem": "npm", "requirement": "0.1.1"})
            self.assertEqual(result["state"], "invalid_metadata_or_archive")

    def test_changed_wire_schema_is_detected_against_package_metadata(self):
        npm = json.loads((gate.ROOT / "packages/lenso-agent-tool-sdk/package.json").read_text())
        npm["lenso"]["build"]["provider_contracts"][0]["descriptor_digest"] = "sha256:" + "0" * 64
        # Fake only the codegen subprocess; use real source metadata/Descriptor.
        def generate(args, **_kwargs):
            digest = "sha256:" + "1" * 64
            Path(args[-1]).write_text(f'export const DESCRIPTOR_DIGEST = "{digest}";')
            return type("Completed", (), {"returncode": 0})()
        with patch.object(gate.subprocess, "run", side_effect=generate), self.assertRaisesRegex(ValueError, "build API metadata"):
            gate.api_evidence(gate.ROOT, npm, Path("unused-codegen"))

    def test_published_archive_requires_integrity_and_source_api_metadata(self):
        import base64
        import hashlib
        manifest = {"name": gate.NPM_SDK, "version": "0.1.1", "exports": {}, "lenso": {"build": "wrong"}}
        contents = json.dumps(manifest).encode()
        target = io.BytesIO()
        with tarfile.open(fileobj=target, mode="w:gz") as archive:
            member = tarfile.TarInfo("package/package.json")
            member.size = len(contents)
            archive.addfile(member, io.BytesIO(contents))
        body = target.getvalue()
        item = {"name": gate.NPM_SDK, "ecosystem": "npm", "requirement": "0.1.1", "archive_fields": {"lenso": {"build": "expected"}}}
        metadata = {"dist": {"tarball": "https://registry.npmjs.org/@lenso/agent-tool-sdk/-/agent-tool-sdk-0.1.1.tgz", "integrity": "sha512-invalid"}}
        with patch.object(gate, "fetch", return_value=({"state": "received"}, body)):
            self.assertEqual(gate.verify_published(item, metadata)["state"], "integrity_mismatch")
            metadata["dist"]["integrity"] = "sha512-" + base64.b64encode(hashlib.sha512(body).digest()).decode()
            with self.assertRaisesRegex(ValueError, "published API metadata"):
                gate.verify_published(item, metadata)
            item["archive_fields"] = {"lenso": manifest["lenso"]}
            self.assertEqual(gate.verify_published(item, metadata)["state"], "archive_verified")


if __name__ == "__main__":
    unittest.main()
