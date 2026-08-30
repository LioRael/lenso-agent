#!/usr/bin/env python3
"""Render one ACP Registry entry from versioned release archive checksums."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


TARGETS = (
    "darwin-aarch64",
    "darwin-x86_64",
    "linux-aarch64",
    "linux-x86_64",
    "windows-aarch64",
    "windows-x86_64",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--artifacts", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args()


def checksum(path: Path, expected_archive: str) -> str:
    fields = path.read_text(encoding="utf-8").strip().split()
    if len(fields) != 2 or fields[1] != expected_archive:
        raise ValueError(f"invalid checksum record: {path}")
    if re.fullmatch(r"[0-9a-f]{64}", fields[0]) is None:
        raise ValueError(f"invalid SHA-256 digest: {path}")
    return fields[0]


def main() -> None:
    args = parse_args()
    if re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version) is None:
        raise ValueError("version must be an exact semantic version")
    if not args.base_url.startswith("https://") or "/latest/" in args.base_url:
        raise ValueError("base URL must be versioned HTTPS, not a latest alias")

    binary: dict[str, object] = {}
    for target in TARGETS:
        archive = f"lenso-agent-acp-v{args.version}-{target}.tar.gz"
        archive_path = args.artifacts / archive
        checksum_path = args.artifacts / f"{archive}.sha256"
        if not archive_path.exists() and not checksum_path.exists():
            continue
        if not archive_path.is_file() or not checksum_path.is_file():
            raise ValueError(f"archive and checksum must both exist: {archive}")
        binary[target] = {
            "archive": f"{args.base_url.rstrip('/')}/{archive}",
            "sha256": checksum(checksum_path, archive),
            "cmd": "lenso-agent-acp.exe" if target.startswith("windows-") else "./lenso-agent-acp",
        }

    if not binary:
        raise ValueError("at least one packaged ACP binary is required")

    entry = {
        "id": "lenso",
        "name": "Lenso Agent",
        "version": args.version,
        "description": "A local coding Agent with replaceable Lenso Plugins",
        "repository": "https://github.com/LioRael/lenso-agent",
        "authors": ["Lenso contributors"],
        "license": "MIT",
        "distribution": {"binary": binary},
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(entry, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
