#!/usr/bin/env python3
"""Check the minimal Console binary without App Plugin feature unification."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import threading
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

class CatalogHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"models": [{
            "slug": "gpt-5.6-luna", "display_name": "Fixture", "description": "Fixture",
            "default_reasoning_level": "medium", "supported_reasoning_levels": [
                {"effort": "medium", "description": "Fixture"}], "visibility": "list",
            "additional_speed_tiers": [], "service_tiers": [], "default_service_tier": None,
            "supports_parallel_tool_calls": True, "context_window": 272000,
            "effective_context_window_percent": 95, "input_modalities": ["text"],
        }]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


binary = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory() as home, socket.socket() as listener:
    listener.bind(("127.0.0.1", 0))
    port = listener.getsockname()[1]
    listener.close()
    catalog = ThreadingHTTPServer(("127.0.0.1", 0), CatalogHandler)
    threading.Thread(target=catalog.serve_forever, daemon=True).start()
    credential = Path(home) / "fixture-auth.json"
    credential.write_text(json.dumps({"openai-codex": {"type": "oauth",
        "access": "fixture-access", "refresh": "fixture-refresh", "accountId": "fixture",
        "expires": 18446744073709551615}}))
    for plugin, instance, content in [
        ("auth.openai-codex", "auth", f'credential_file = "{credential}"\n'),
        ("model.openai-codex-direct", "model", f'base_url = "http://127.0.0.1:{catalog.server_port}"\nmodel = "gpt-5.6-luna"\nreasoning_effort = "medium"\n'),
    ]:
        directory = Path(home) / "plugins" / f"lenso.agent.{plugin}"
        directory.mkdir(parents=True)
        (directory / f"{instance}.toml").write_text(content)
    environment = dict(os.environ, LENSO_AGENT_HOME=home,
                       LENSO_AGENT_CONTROL_TOKEN="isolated-local-check")
    process = subprocess.Popen([
        str(binary), "--listen", f"127.0.0.1:{port}", "--plugin-control",
        "--plugin-configuration-store", str(Path(home) / "configuration.sqlite3"),
    ], env=environment, cwd=home, stdout=subprocess.DEVNULL)
    try:
        url = f"http://127.0.0.1:{port}/api/console/v1/agent/bootstrap"
        for attempt in range(100):
            if process.poll() is not None:
                raise RuntimeError("Console exited before readiness")
            try:
                with urllib.request.urlopen(url, timeout=1) as response:
                    bootstrap = json.load(response)
                break
            except (urllib.error.URLError, TimeoutError):
                time.sleep(0.1)
        else:
            raise RuntimeError("Console did not become ready")
        capabilities = bootstrap["capabilities"]
        assert capabilities["profileImport"] is False, capabilities
        assert capabilities["profileSelection"] is False, capabilities
        base = f"http://127.0.0.1:{port}/api/console/v1/agent"
        with urllib.request.urlopen(base + "/plugins", timeout=5) as response:
            inventory = json.load(response)
        request = urllib.request.Request(base + "/control/profiles/import", method="POST",
            headers={"Authorization": "Bearer isolated-local-check", "Content-Type": "application/json"},
            data=json.dumps({"expectedRevision": "unused", "expectedStreamId": inventory["streamId"]}).encode())
        try:
            urllib.request.urlopen(request, timeout=5)
            raise AssertionError("minimal Console imported unsupported coding Profiles")
        except urllib.error.HTTPError as error:
            assert error.code == 409, error.code
            assert "Plugin inventory" in error.read().decode()
        assert not (Path(home) / "profiles" / "code.toml").exists()
        print("PASS: minimal Console hides and rejects unsupported coding Profiles")
    finally:
        catalog.shutdown()
        catalog.server_close()
        process.send_signal(signal.SIGINT)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
