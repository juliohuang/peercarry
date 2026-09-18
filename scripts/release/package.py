#!/usr/bin/env python3
"""Create the raw tray/CLI release files and a first-install archive."""
from __future__ import annotations

import argparse
import hashlib
import shutil
import tarfile
import tempfile
import zipfile
from pathlib import Path


def readme(version: str, triple: str, platform: str) -> str:
    return f"""# peercarry {version} ({triple})

This archive contains the `peercarry-tray` desktop application and the `peercarry` CLI.

## First install

1. Run the platform installer (`Install.ps1` on Windows or `install.sh` on
   Linux/macOS), or extract this archive to a user-owned directory.
2. Windows installer starts the tray and enables login startup automatically.
   On Linux/macOS, follow the printed `peercarry install-service` command to enable the
   per-user service/startup entry. The command writes only user-level service
   configuration (systemd on Linux, launchd on macOS, and the HKCU Run key on
   Windows).
3. To remove the startup entry later, run `peercarry install-service --remove`.

Platform: {platform}. The first run may require the normal desktop clipboard
permissions for your operating system.
"""


INSTALL_PS1 = Path(__file__).with_name("Install.ps1").read_text(encoding="utf-8")
INSTALL_SH = Path(__file__).with_name("install.sh").read_text(encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--triple", required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--tray", type=Path, required=True)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()
    out = args.out_dir.resolve()
    out.mkdir(parents=True, exist_ok=True)
    suffix = ".zip" if "windows" in args.triple else ".tar.gz"
    archive = out / f"peercarry-{args.version}-{args.triple}{suffix}"
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp) / f"peercarry-{args.version}-{args.triple}"
        root.mkdir()
        shutil.copy2(args.tray, root / args.tray.name)
        shutil.copy2(args.cli, root / args.cli.name)
        (root / "README-INSTALL.md").write_text(
            readme(args.version, args.triple, args.platform), encoding="utf-8"
        )
        (root / "Install.ps1").write_text(INSTALL_PS1, encoding="utf-8")
        (root / "install.sh").write_text(INSTALL_SH, encoding="utf-8")
        (root / "install.sh").chmod(0o755)
        if suffix == ".zip":
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
                for item in root.rglob("*"):
                    zf.write(item, item.relative_to(Path(temp)))
        else:
            with tarfile.open(archive, "w:gz") as tf:
                tf.add(root, root.name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    print(f"created {archive.name} sha256={digest}")


if __name__ == "__main__":
    main()
