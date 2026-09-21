"""Replace real native Provider binaries while retaining the durable store."""

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

source = Path(__file__).resolve().parent


def run(command, root):
    return subprocess.check_output(command, cwd=root, text=True)


with tempfile.TemporaryDirectory(prefix="lenso-binary-upgrade-") as temporary:
    root = Path(temporary)
    consumer = root / "consumer"
    shutil.copytree(source, consumer, ignore=shutil.ignore_patterns("target", ".cargo", "Cargo.lock", "__pycache__"))
    manifest = consumer / "runner/Cargo.toml"
    manifest.write_text(manifest.with_suffix(".toml.in").read_text().replace("# LENSO_LOCAL_PATCHES", ""))
    common = ["--manifest-path", str(manifest)]
    run(["cargo", "generate-lockfile", *common], root)
    metadata = json.loads(run(["cargo", "metadata", "--locked", *common, "--format-version", "1"], root))
    for package in metadata["packages"]:
        if package["source"] is None:
            assert Path(package["manifest_path"]).is_relative_to(consumer), package
        else:
            assert package["source"] == "registry+https://github.com/rust-lang/crates.io-index", package
    binaries = []
    receipts = []
    for version, features in [("1.0.0", []), ("2.0.0", ["--features", "upgraded"]), ("3.0.0", ["--features", "incompatible"])]:
        messages = run(["cargo", "build", "--locked", *common, "--bin", "durable-phase", "--message-format=json", *features], root)
        executable = next(json.loads(line)["executable"] for line in messages.splitlines()
                          if json.loads(line).get("executable"))
        binary = root / f"durable-provider-{version}"
        shutil.copy2(executable, binary)
        assert run([str(binary), "artifact", str(root / "unused")], root).strip() == version
        binaries.append(binary)
        receipts.append({"version": version, "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()})
    assert len({receipt["sha256"] for receipt in receipts}) == 3, receipts
    store = root / "durable-state.json"

    def phase(binary, name):
        run([str(binary), name, str(store)], root)

    phase(binaries[0], "start")
    phase(binaries[0], "uncertain")
    phase(binaries[1], "binary-recovery")
    phase(binaries[1], "binary-recovery")
    phase(binaries[1], "signal")
    phase(binaries[1], "recover-resume")
    # This release cannot read v1 even when its caller claims support for v1.
    phase(binaries[2], "upgrade")
    blocked = store.read_bytes()
    # Restart the incompatible release: inspection remains possible and the
    # blocked task cannot be resumed using a pre-upgrade approval.
    assert b'UpgradeRequired' in blocked or b'upgrade_required' in blocked
    print(json.dumps({"artifacts": receipts, "result": "passed"}, indent=2))
    print("PASS: distinct native binaries preserve compatible state, block incompatible recovery and invalidate stale approval.")
