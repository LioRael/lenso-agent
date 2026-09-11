#!/usr/bin/env python3
"""Exercise real TUI startup and autocomplete with saturated default snapshots."""
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import struct
import sys
import tempfile
import termios
import time

binary = str(Path(sys.argv[1]).resolve(strict=True))
with tempfile.TemporaryDirectory(prefix="lenso-tui-startup-") as temporary:
    root = Path(temporary)
    workspace = root / "workspace"
    workspace.mkdir()
    for index in range(2200):
        (workspace / f"file{index:04}.txt").write_text("fixture\n")
    for index in range(256):
        name = f"skill-{index:03}"
        skill = workspace / ".agents" / "skills" / name
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text(
            f"---\nname: {name}\ndescription: Startup fixture\n---\nNo actions.\n"
        )
    # Select a local deterministic model so startup needs neither credentials nor network.
    model = root / "agent" / "plugins" / "lenso.agent.model.fixture"
    model.mkdir(parents=True)
    (model / "model.toml").write_text('model = "fixture/readme-summary-v1"\n')
    profiles = root / "agent" / "profiles"
    profiles.mkdir()
    (profiles / "startup.toml").write_text('instances = ["lenso.agent.model.fixture/model"]\n')
    (root / "home").mkdir()
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith("LENSO_")}
    environment.update(HOME=str(root / "home"), LENSO_AGENT_HOME=str(root / "agent"),
                       TERM="xterm-256color")
    pid, terminal = pty.fork()
    if pid == 0:
        os.chdir(workspace)
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
        os.execve(binary, [binary, "--profile", "startup"], environment)
    output = bytearray()
    stage = 0
    reaped = False
    deadline = time.monotonic() + 45
    try:
        while time.monotonic() < deadline:
            if select.select([terminal], [], [], 0.1)[0]:
                try:
                    chunk = os.read(terminal, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    chunk = b""
                if chunk:
                    output.extend(chunk)
                    if b"\x1b[6n" in chunk:
                        os.write(terminal, b"\x1b[1;1R")
            if stage == 0 and b"shortcuts" in output:
                os.write(terminal, b"/skill-255")
                output.clear()
                stage = 1
            elif stage == 1 and b"Startup fixture" in output:
                os.write(terminal, b"\x7f" * len("/skill-255") + b"@file1000")
                output.clear()
                stage = 2
            elif stage == 2 and b"Workspace file" in output:
                os.write(terminal, b"\x03")
                stage = 3
            finished, status = os.waitpid(pid, os.WNOHANG)
            if finished:
                reaped = True
                assert stage == 3 and os.waitstatus_to_exitcode(status) == 0, (
                    f"TUI startup/autocomplete failed at stage {stage}: "
                    + output.decode(errors="replace")[-4000:]
                )
                print("TUI startup, Skill completion, file completion, and clean exit passed")
                break
        else:
            raise RuntimeError(f"TUI smoke timed out at stage {stage}: "
                               + output.decode(errors="replace")[-4000:])
    finally:
        if not reaped:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        os.close(terminal)
