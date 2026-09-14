#!/usr/bin/env python3
"""Report uncompressed release sizes, including the terminal installation total."""

import argparse
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    names = ("lenso-agent", "lenso-agent-cli", "lenso-agent-acp")
    sizes = {name: (args.directory / name).stat().st_size for name in names}
    web_names = ("lenso-agent-web", "lenso-agent-console-web")
    for name in web_names:
        path = args.directory / name
        if path.is_file():
            sizes[name] = path.stat().st_size
    report = {
        "unit": "bytes",
        "binaries": sizes,
        "terminal_installation": sizes["lenso-agent"] + sizes["lenso-agent-cli"],
        "terminal_with_acp": sum(sizes[name] for name in names),
    }
    if all(name in sizes for name in web_names):
        report["default_installation"] = report["terminal_installation"] + sum(
            sizes[name] for name in web_names
        )
        report["all_release_entrypoints"] = sum(sizes.values())
    for name, size in sizes.items():
        print(f"{name}: {size:,} bytes ({size / 1_000_000:.2f} MB)")
    print(f"Terminal installation: {report['terminal_installation']:,} bytes")
    print(f"Terminal with ACP: {report['terminal_with_acp']:,} bytes")
    if "default_installation" in report:
        print(f"Default installation (including Web): {report['default_installation']:,} bytes")
        print(f"All release entrypoints: {report['all_release_entrypoints']:,} bytes")
    if args.output:
        args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
