"""Archive contract checks using inert fixture binaries, no installation."""
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path


class PackageTests(unittest.TestCase):
    def check_package(self, triple, platform, windows=False):
        with tempfile.TemporaryDirectory() as temp:
            folder = Path(temp)
            suffix = ".exe" if windows else ""
            binaries = {"peercarry-tray" + suffix: b"tray fixture", "peercarry" + suffix: b"cli fixture"}
            for name, content in binaries.items():
                (folder / name).write_bytes(content)
            subprocess.run([
                sys.executable, str(Path(__file__).with_name("package.py")),
                "--version", "0.0.0-test", "--triple", triple, "--platform", platform,
                "--tray", str(folder / ("peercarry-tray" + suffix)),
                "--cli", str(folder / ("peercarry" + suffix)), "--out-dir", str(folder / "out"),
            ], check=True, capture_output=True)
            archive = next((folder / "out").iterdir())
            root = "peercarry-0.0.0-test-" + triple
            expected = set(binaries) | {"README-INSTALL.md", "Install.ps1" if windows else "install.sh"}
            if windows:
                with zipfile.ZipFile(archive) as package:
                    self.assertEqual(set(package.namelist()), {root + "/" + name for name in expected})
                    for name, content in binaries.items():
                        self.assertEqual(package.read(root + "/" + name), content)
            else:
                with tarfile.open(archive) as package:
                    files = [member for member in package.getmembers() if member.isfile()]
                    self.assertEqual({member.name for member in files}, {root + "/" + name for name in expected})
                    for member in files:
                        name = Path(member.name).name
                        self.assertEqual(member.mode, 0o644 if name == "README-INSTALL.md" else 0o755)
                        content = package.extractfile(member).read()
                        if name in binaries:
                            self.assertEqual(content, binaries[name])
                        if name == "install.sh":
                            self.assertTrue(content.startswith(b"#!/usr/bin/env sh\n"))
                            self.assertNotIn(b"\r", content)

    def test_windows_archive(self):
        self.check_package("x86_64-pc-windows-msvc", "windows", True)

    def test_macos_archive(self):
        self.check_package("aarch64-apple-darwin", "macos")

    def test_linux_archive(self):
        self.check_package("x86_64-unknown-linux-gnu", "linux")


if __name__ == "__main__":
    unittest.main()
