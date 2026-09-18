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

Before upgrading, quit the tray and stop the existing user service. Never run
the tray and a separate daemon against the same data directory.
Install and sign in to Tailscale on each peer before using cross-device transfer.
Linux desktop use requires GTK 3, libxdo and WebKitGTK 4.1 runtime libraries;
file clipboard support also requires xclip (X11) or wl-clipboard (Wayland).
Linux startup runs the CLI daemon; do not start the tray alongside that service.
macOS builds are standalone executables, not a signed/notarized .app or DMG.

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
        if suffix == ".zip":
            (root / "Install.ps1").write_text(INSTALL_PS1, encoding="utf-8")
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
                for item in root.rglob("*"):
                    zf.write(item, item.relative_to(Path(temp)))
        else:
            (root / "install.sh").write_bytes(INSTALL_SH.encode("utf-8"))
            # Preserve Unix permissions even when packaging on Windows.
            def unix_mode(info: tarfile.TarInfo) -> tarfile.TarInfo:
                info.mode = 0o644 if Path(info.name).name == "README-INSTALL.md" else 0o755
                return info

            with tarfile.open(archive, "w:gz") as tf:
                tf.add(root, root.name, filter=unix_mode)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    print(f"created {archive.name} sha256={digest}")


if __name__ == "__main__":
    main()
