"""Read-only entrypoint checks for explicitly selected active shell examples.

This is deliberately not a shell interpreter. Each fenced block starts in the
configured cwd; only a standalone literal cd changes it. No command is executed.
"""

from __future__ import annotations

import json
import re
import shlex
from pathlib import Path

RUNNERS = {"python", "python3", "node", "bash", "sh"}
SHELLS = {"sh", "bash", "shell"}
FENCE = re.compile(r"^\s*(`{3,}|~{3,})\s*([^\s]*)\s*$")
DYNAMIC = re.compile(r"[$`]")
CONTROL = {"if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "function", "pushd", "popd", "source", ".", "eval"}


def _inside(root: Path, cwd: Path | None, value: str) -> Path | None:
    if DYNAMIC.search(value) or any(c in value for c in "*?[]~"):
        return None
    path = Path(value)
    if not path.is_absolute():
        if cwd is None:
            return None
        path = cwd / path
    path = path.resolve()
    return path if path.is_relative_to(root) else None


def check_documents(repo_root, documents) -> list[dict]:
    """Check paths or {path, cwd} specs, relative to a single repository root.

    Every result has id/status/code/message/locations/remediation. Coverage rows
    additionally expose assessed_count and unassessed_count. Unknown coverage
    never claims that other CLI commands or runtime behavior were validated.
    """
    root = Path(repo_root).resolve()
    rows = []
    for spec in documents:
        if isinstance(spec, str):
            spec = {"path": spec, "cwd": "."}
        relative = spec.get("path", "")
        doc = _inside(root, root, relative)
        configured = _inside(root, root, spec.get("cwd", "."))
        initial_cwd = configured if configured and configured.is_dir() else None
        location = str(doc.relative_to(root)) if doc else "<document outside repository>"
        assessed = 0
        unassessed = 0
        uncertain = False

        def emit(line, status, code, message, remediation, **extra):
            nonlocal uncertain
            uncertain = uncertain or status == "unknown"
            rows.append({"id": f"document:{location}:{line}:{code}", "status": status,
                         "code": code, "message": message,
                         "locations": [f"{location}:{line}"], "remediation": remediation,
                         **extra})

        if not doc or not doc.is_file():
            emit(1, "fail", "document_missing", "Configured active document is missing or outside the repository.",
                 "Select an existing repository-relative active document.")
            continue
        try:
            lines = doc.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError):
            emit(1, "unknown", "document_unreadable", "Cannot read the configured document as UTF-8.",
                 "Restore a readable UTF-8 document and rerun the check.")
            continue
        fence = None
        fence_line = 0
        active = False
        cwd = initial_cwd
        pending = ""
        pending_line = 0
        heredoc_end = None
        for number, raw in enumerate(lines, 1):
            marker = FENCE.match(raw)
            if marker:
                delimiter, language = marker.groups()
                if fence is None:
                    fence = delimiter
                    fence_line = number
                    active = language in SHELLS
                    cwd = initial_cwd
                elif delimiter[0] == fence[0] and len(delimiter) >= len(fence) and not language:
                    if active and pending:
                        assessed += 1
                        emit(pending_line, "unknown", "shell_ambiguous", "Unfinished shell continuation.",
                             "Complete the example before checking its entrypoint.")
                    fence, active, pending, heredoc_end = None, False, "", None
                continue
            if not active:
                continue
            if heredoc_end is not None:
                if raw.strip() == heredoc_end:
                    heredoc_end = None
                continue
            stripped = raw.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if not pending:
                pending_line = number
            pending += stripped
            if pending.endswith("\\"):
                pending = pending[:-1] + " "
                continue
            command, pending = pending, ""
            number = pending_line
            heredoc = re.search(r"<<-?\s*['\"]?([A-Za-z_][A-Za-z0-9_]*)", command)
            if heredoc:
                heredoc_end = heredoc.group(1)
            try:
                lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|<>()")
                lexer.whitespace_split = True
                tokens = list(lexer)
            except ValueError:
                tokens = []
            if not tokens:
                assessed += 1
                emit(number, "unknown", "shell_ambiguous", "Cannot parse this shell example safely.",
                     "Use a standalone literal command or review this example manually.")
                continue
            name = tokens[0]
            if (heredoc or DYNAMIC.search(command) or name in CONTROL
                    or any(token and all(c in ";&|<>()" for c in token) for token in tokens)):
                assessed += 1
                # Shell source, control flow, substitutions and compound commands
                # can change cwd; do not infer the later command's location.
                cwd = None
                emit(number, "unknown", "shell_ambiguous", "Variables, here-docs, control flow or compound shell require review.",
                     "Use a separate literal command and explicit cwd, or retain manual evidence.")
                continue
            if name == "cd":
                assessed += 1
                target = _inside(root, cwd, tokens[1]) if len(tokens) == 2 and tokens[1] != "-" else None
                if target is None:
                    cwd = None
                    emit(number, "unknown", "document_cwd_unknown", "Cannot resolve this cd within the repository.",
                         "Use a literal repository directory and configure the block's initial cwd.")
                elif not target.is_dir():
                    cwd = None
                    emit(number, "fail", "document_cwd_missing", "The documented cd directory does not exist.",
                         "Correct the directory before running subsequent commands.")
                else:
                    cwd = target
                    emit(number, "pass", "document_cwd_exists", "Literal cd resolves to an existing repository directory.", "None.")
                continue
            if name in RUNNERS:
                assessed += 1
                if len(tokens) < 2 or tokens[1].startswith("-"):
                    emit(number, "unknown", "script_entrypoint_unknown", "Interpreter options or inline/module execution have no checked local script entrypoint.",
                         "Name a literal script path or review the module/inline example separately.")
                    continue
                target = _inside(root, cwd, tokens[1])
                if target is None:
                    emit(number, "unknown", "script_entrypoint_unknown", "Script path or cwd cannot be resolved within the repository.",
                         "Use a literal script path and an explicit existing repository cwd.")
                else:
                    exists = target.is_file()
                    emit(number, "pass" if exists else "fail", "script_entrypoint_exists" if exists else "script_entrypoint_missing",
                         f"Documented script {'exists' if exists else 'does not exist'}: {target.relative_to(root)}.",
                         "None." if exists else "Update the command to the current script path or restore the intended script.")
                continue
            if name in {"npm", "bun"} and len(tokens) > 1 and tokens[1] == "run":
                assessed += 1
                manager_args = tokens[2:tokens.index("--")] if "--" in tokens else tokens[2:]
                changes_package = any(arg.split("=", 1)[0] in {"--cwd", "--prefix", "--filter", "--workspace", "--workspaces", "-w"}
                                      for arg in manager_args)
                if cwd is None or len(tokens) < 3 or tokens[2].startswith("-") or changes_package:
                    emit(number, "unknown", "package_script_unknown", "Cannot resolve package cwd or a literal npm/Bun script name.",
                         "Configure cwd and use an explicit script name; review workspace/filter options separately.")
                    continue
                manifest = cwd / "package.json"
                try:
                    package = json.loads(manifest.read_text(encoding="utf-8"))
                    scripts = package.get("scripts", {})
                    if not isinstance(scripts, dict):
                        raise ValueError("scripts must be an object")
                except FileNotFoundError:
                    if name == "npm":
                        emit(number, "fail", "package_manifest_missing", "No package.json exists at the documented cwd.",
                             "Select the package directory before invoking npm run.")
                        continue
                    scripts = {}
                except (OSError, UnicodeError, ValueError, AttributeError):
                    emit(number, "unknown", "package_manifest_unreadable", "Cannot inspect package.json scripts at this cwd.",
                         "Repair package.json and rerun the entrypoint check.")
                    continue
                script = scripts.get(tokens[2])
                exists = isinstance(script, str) and bool(script.strip())
                if name == "bun" and not exists:
                    entry = tokens[2]
                    target = _inside(root, cwd, entry)
                    explicit_file = "/" in entry or Path(entry).suffix in {".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".mts", ".cts"}
                    if target is not None and target.is_file():
                        emit(number, "pass", "script_entrypoint_exists",
                             f"Documented Bun file exists: {target.relative_to(root)}.", "None.")
                    elif explicit_file and target is not None:
                        emit(number, "fail", "script_entrypoint_missing",
                             f"Documented Bun file does not exist: {target.relative_to(root)}.",
                             "Correct the local file path or restore the intended file.")
                    else:
                        emit(number, "unknown", "bun_entrypoint_unknown",
                             "Bun entrypoint is not a checked package script or local file; bin resolution is unverified.",
                             "Verify the bin provider and installed dependency separately, or use an explicit repository file path.")
                    continue
                emit(number, "pass" if exists else "fail", "package_script_exists" if exists else "package_script_missing",
                     f"package.json {'declares' if exists else 'does not declare'} script {tokens[2]}.",
                     "None." if exists else "Use a script declared by the selected package or add the intended script.")
                continue
            unassessed += 1
        if active and fence is not None:
            if pending:
                assessed += 1
                emit(pending_line, "unknown", "shell_ambiguous", "Unfinished shell continuation at end of document.",
                     "Complete the command and close the shell fence before checking its entrypoint.")
            emit(fence_line, "unknown", "shell_fence_unclosed", "Shell code fence is not closed at end of document.",
                 "Close the shell code fence and rerun the entrypoint check.")
        emit(1, "unknown" if uncertain or unassessed or not assessed else "pass", "document_entrypoint_coverage",
             f"Checked {assessed} scoped entrypoints/cwd statements; {unassessed} other commands remain unassessed. No commands were executed.",
             "Run the owner CLI/help/runtime checks separately; this check proves only selected local entrypoints.",
             assessed_count=assessed, unassessed_count=unassessed)
    return rows
