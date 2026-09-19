"""Run convention output through an isolated App Agent, including denied calls."""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("--agent-cli", required=True, type=Path)
parser.add_argument("--app-dist", required=True, type=Path)
args = parser.parse_args()
agent = args.agent_cli.resolve()
dist = args.app_dist.resolve()
with tempfile.TemporaryDirectory(prefix="lenso-convention-agent-") as temporary:
    home = Path(temporary)
    for instance in ("agent", "researcher", "reviewer"):
        config = home / "plugins/lenso.agent.loop" / f"{instance}.toml"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text('model = "fixture/readme-summary-v1"\ntool_allowlist = ["greet"]\n')
    config = home / "plugins/lenso.agent.model.fixture/model.toml"
    config.parent.mkdir(parents=True)
    config.write_text('model = "fixture/readme-summary-v1"\n')
    environment = dict(os.environ, LENSO_AGENT_HOME=str(home))
    checked = subprocess.run(
        [str(agent), "dx", "check", "--from", str(dist)],
        cwd=home,
        env=environment,
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert checked.returncode == 0, checked.stderr
    deployments = json.loads(checked.stdout)
    assert len(deployments) == 1 and deployments[0]["profile"] == "greeter", deployments
    installed = subprocess.run(
        [str(agent), "dx", "apply", "--from", str(dist)],
        cwd=home,
        env=environment,
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert installed.returncode == 0, installed.stderr
    receipt = json.loads(installed.stdout)
    assert receipt == [{"plugin_id": deployments[0]["plugin_id"], "instance": "default", "profile": "greeter"}], receipt
    plugin = home / "plugins" / deployments[0]["plugin_id"]
    bundle = plugin / "plugin.lenso-plugin"
    assert (plugin / "default.disabled").is_file(), "DX must not activate a Tool in the default Agent"
    def run(prompt, *flags):
        return subprocess.run([str(agent), "--prompt", prompt, *flags], cwd=home, env=environment, capture_output=True, text=True, timeout=60)
    unselected = run("Invoke the convention greet tool.", "--allow-tool", "greet")
    assert unselected.returncode != 0, "Default Agent must not receive an App Tool implicitly"
    allowed = run("Invoke the convention greet tool.", "--profile", "greeter", "--allow-tool", "greet")
    assert allowed.returncode == 0, allowed.stderr
    assert allowed.stdout.strip(), allowed.stdout
    denied = run("Attempt the denied convention greet tool.", "--profile", "greeter", "--no-tools")
    assert denied.returncode != 0 and "tool_not_allowed" in denied.stderr, denied.stderr
    with sqlite3.connect(home / "sessions.sqlite3") as database:
        rows = [(kind, json.loads(payload)) for kind, payload in database.execute("SELECT kind,payload_json FROM events")]
    requests = [payload for kind, payload in rows if kind == "model_requested"]
    assert requests[0]["tool_count"] == 1 and requests[-1]["tool_count"] == 0
    results = [payload for kind, payload in rows if kind == "tool_result"]
    assert len(results) == 1 and results[0]["status"] == "completed" and results[0]["content"], "Authorized tool did not execute"
    bundle.rename(home / "removed-plugin.lenso-plugin")
    removed = run("Invoke the convention greet tool.", "--profile", "greeter", "--allow-tool", "greet")
    assert removed.returncode != 0, "Disabled provider must not remain callable"
    print("PASS: checked and applied Agent DX resource, explicit Profile policy, authorized tool execution, denied model call, and provider removal")
