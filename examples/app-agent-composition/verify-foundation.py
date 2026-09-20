"""Build an App outside the framework workspace and import its Agent contributions."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


parser = argparse.ArgumentParser()
parser.add_argument("--engine-host", required=True, type=Path)
parser.add_argument("--agent-cli", required=True, type=Path)
parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parent)
parser.add_argument(
    "--support",
    type=Path,
    default=Path(__file__).resolve().parents[2] / "packages/agent-tool-convention",
)
args = parser.parse_args()

engine = args.engine_host.resolve()
agent = args.agent_cli.resolve()
source = args.source.resolve()
support = args.support.resolve()
assert engine.is_file(), engine
assert agent.is_file(), agent
assert (support / "package.json").is_file(), support
assert (support / "compiler.mjs").is_file(), support


def run(arguments, *, cwd, environment=None):
    completed = subprocess.run(
        [str(value) for value in arguments],
        cwd=cwd,
        env=environment,
        capture_output=True,
        text=True,
        timeout=300,
    )
    assert completed.returncode == 0, (
        f"command failed: {' '.join(str(value) for value in arguments)}\n"
        f"stdout:\n{completed.stdout}\n"
        f"stderr:\n{completed.stderr}"
    )
    return completed


def read_profile_lists(path: Path) -> dict[str, list[str]]:
    """Read the two JSON-compatible list fields used by this fixture.

    Keeping this deliberately narrow avoids making the verification command
    depend on Python 3.11's ``tomllib`` when the documented command is simply
    ``python3`` on a stock macOS installation.
    """

    fields: dict[str, list[str]] = {}
    for line in path.read_text().splitlines():
        key, separator, value = line.partition("=")
        key = key.strip()
        if separator and key in {"instances", "allowed_tools"}:
            decoded = json.loads(value.strip())
            assert isinstance(decoded, list) and all(
                isinstance(item, str) for item in decoded
            ), path
            fields[key] = decoded
    return fields


with tempfile.TemporaryDirectory(prefix="lenso-agent-foundation-external-") as temporary:
    root = Path(temporary) / "consumer"
    shutil.copytree(source / "app", root / "app")
    shutil.copytree(source / "plugins", root / "plugins")
    root.mkdir(exist_ok=True)
    (root / "lenso.toml").write_text(
        f"plugin_sources = [{json.dumps(str(support))}]\n"
    )
    distribution = root / "dist"
    run(
        [engine, "app", "build", "--root", root, "--out", distribution],
        cwd=root,
    )

    resources = json.loads((distribution / "resources.json").read_text())
    bundles = json.loads((distribution / "bundles.json").read_text())
    assert resources["schema"] == "lenso.app-resources.v1", resources
    deployments = [
        resource
        for resource in resources["resources"]
        if resource["schema"].startswith("lenso.agent.deployment@")
    ]
    tool_deployments = [
        resource
        for resource in deployments
        if resource["schema"] == "lenso.agent.deployment@1"
    ]
    profile_deployments = [
        resource
        for resource in deployments
        if resource["schema"] == "lenso.agent.deployment@2"
    ]
    assert len(tool_deployments) == 2, deployments
    assert len(profile_deployments) == 1, deployments
    bundle_ids = {bundle["plugin_id"] for bundle in bundles}
    assert {resource["owner"] for resource in tool_deployments} <= bundle_ids
    assert profile_deployments[0]["owner"] not in bundle_ids

    home = root / "agent-home"
    for instance in ("agent", "researcher", "reviewer"):
        config = home / "plugins/lenso.agent.loop" / f"{instance}.toml"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text(
            'model = "fixture/readme-summary-v1"\n'
            'tool_allowlist = ["lookup_order", "lookup_billing"]\n'
        )
    fixture_model = home / "plugins/lenso.agent.model.fixture/model.toml"
    fixture_model.parent.mkdir(parents=True, exist_ok=True)
    fixture_model.write_text('model = "fixture/readme-summary-v1"\n')
    environment = dict(os.environ, LENSO_AGENT_HOME=str(home))

    checked = run(
        [agent, "dx", "check", "--from", distribution],
        cwd=root,
        environment=environment,
    )
    inspection = json.loads(checked.stdout)
    assert len(inspection) == 3, inspection
    inspected_tools = [item for item in inspection if item.get("plugin_id")]
    inspected_profile = [item for item in inspection if item.get("contribution_id")]
    assert len(inspected_tools) == 2, inspection
    assert inspected_profile == [
        {
            "contribution_id": profile_deployments[0]["owner"],
            "profile": "assistant",
            "resource_path": profile_deployments[0]["path"],
        }
    ], inspection

    installed = run(
        [agent, "dx", "apply", "--from", distribution],
        cwd=root,
        environment=environment,
    )
    receipt = json.loads(installed.stdout)
    applied_tools = [item for item in receipt if item.get("plugin_id")]
    applied_profile = [item for item in receipt if item.get("contribution_id")]
    assert len(applied_tools) == 2, receipt
    assert applied_profile == [
        {
            "contribution_id": profile_deployments[0]["owner"],
            "profile": "assistant",
        }
    ], receipt
    for item in applied_tools:
        plugin = home / "plugins" / item["plugin_id"]
        assert (plugin / "plugin.lenso-plugin").is_dir(), plugin
        assert (plugin / "default.disabled").is_file(), plugin
    assert not (home / "plugins" / profile_deployments[0]["owner"]).exists()

    profile = read_profile_lists(home / "profiles/assistant.toml")
    assert profile["instances"] == ["lenso.agent.model.fixture/model"], profile
    assert profile["allowed_tools"] == [], profile
    print(
        "PASS: external App built Bun and Rust Tool contributions plus a "
        "Profile-only contribution; only the Tools became disabled Plugins."
    )
