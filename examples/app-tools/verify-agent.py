"""Run convention output through an isolated App Agent, including denied calls."""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument("--agent-cli", required=True, type=Path)
parser.add_argument("--app-dist", required=True, type=Path)
args = parser.parse_args()
agent = args.agent_cli.resolve()
dist = args.app_dist.resolve()
entry = next(item for item in json.loads((dist / "bundles.json").read_text()) if ".surface-" in item["plugin_id"])
with tempfile.TemporaryDirectory(prefix="lenso-convention-agent-") as temporary:
    home = Path(temporary)
    for instance in ("agent", "researcher", "reviewer"):
        config = home / "plugins/lenso.agent.loop" / f"{instance}.toml"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text('model = "fixture/readme-summary-v1"\ntool_allowlist = ["greet"]\n')
    config = home / "plugins/lenso.agent.model.fixture/model.toml"
    config.parent.mkdir(parents=True)
    config.write_text('model = "fixture/readme-summary-v1"\n')
    plugin = home / "plugins" / entry["plugin_id"]
    bundle = plugin / "plugin.lenso-plugin"
    bundle.mkdir(parents=True)
    with zipfile.ZipFile(dist / entry["path"]) as archive:
        for name in archive.namelist():
            if Path(name).is_absolute() or ".." in Path(name).parts:
                raise ValueError("Invalid bundle path")
        archive.extractall(bundle)
    (plugin / "default.toml").write_text("")
    environment = dict(os.environ, LENSO_AGENT_HOME=str(home))
    def run(prompt, *flags):
        return subprocess.run([str(agent), "--prompt", prompt, *flags], cwd=home, env=environment, capture_output=True, text=True, timeout=60)
    allowed = run("Invoke the convention greet tool.", "--allow-tool", "greet")
    assert allowed.returncode == 0, allowed.stderr
    assert "Hello, Ada!" in allowed.stdout, allowed.stdout
    denied = run("Attempt the denied convention greet tool.", "--no-tools")
    assert denied.returncode != 0 and "tool_not_allowed" in denied.stderr, denied.stderr
    with sqlite3.connect(home / "sessions.sqlite3") as database:
        rows = [(kind, json.loads(payload)) for kind, payload in database.execute("SELECT kind,payload_json FROM events")]
    requests = [payload for kind, payload in rows if kind == "model_requested"]
    assert requests[0]["tool_count"] == 1 and requests[-1]["tool_count"] == 0
    assert sum(kind == "tool_result" for kind, _ in rows) == 1, "Denied tool must never execute"
    (plugin / "default.disabled").write_text("")
    removed = run("Invoke the convention greet tool.", "--allow-tool", "greet")
    assert removed.returncode != 0, "Disabled provider must not remain callable"
    print("PASS: real Agent catalog, authorized Bun tool execution, denied model call, and provider removal")
