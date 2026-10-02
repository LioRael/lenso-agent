#!/usr/bin/env python3
"""Prove fixture isolation and preserved stderr when the child fails readiness."""
import os
from pathlib import Path
import subprocess
import tempfile

with tempfile.TemporaryDirectory(prefix="lenso-admission-negative-") as temporary:
    binary = Path(temporary) / "fixture-child"
    binary.write_text('''#!/usr/bin/env python3
import os
from pathlib import Path
import sys
home = Path(os.environ["LENSO_AGENT_HOME"])
assert home == Path.cwd() == Path(os.environ["HOME"])
assert Path(os.environ["CODEX_HOME"]).is_relative_to(home)
assert Path(os.environ["XDG_CONFIG_HOME"]).is_relative_to(home)
assert Path(os.environ["XDG_DATA_HOME"]).is_relative_to(home)
assert "LENSO_AGENT_INHERITED" not in os.environ
assert "LENSO_CONSOLE_INHERITED" not in os.environ
assert Path(sys.argv[sys.argv.index("--plugin-configuration-store") + 1]).is_relative_to(home)
assert os.environ["LENSO_AGENT_CONTROL_TOKEN"] == "isolated-local-check"
print("isolated child deliberately failed before readiness", file=sys.stderr, flush=True)
sys.exit(25)
''')
    binary.chmod(0o755)
    environment = dict(os.environ, LENSO_AGENT_INHERITED="poisoned",
                       LENSO_CONSOLE_INHERITED="poisoned", LENSO_AGENT_HOME="/unusable",
                       LENSO_AGENT_CONTROL_TOKEN="inherited-token")
    result = subprocess.run(
        ["python3", str(Path(__file__).with_name("check-console-profile-admission.py")), str(binary)],
        env=environment, capture_output=True, text=True, timeout=15,
    )
    assert result.returncode != 0, "failed child was accepted as ready"
    assert "isolated child deliberately failed before readiness" in result.stderr, result.stderr
    assert "Console exited before readiness" in result.stderr, result.stderr
    print("PASS: fixture clears inherited configuration, isolates state and retains failed child stderr")
