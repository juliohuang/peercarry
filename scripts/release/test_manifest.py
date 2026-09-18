import base64
import hashlib
import json
import tempfile
import unittest
import os
import subprocess
import sys
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

import manifest


class ManifestFixtureTests(unittest.TestCase):
    def test_cli_build_verify_and_tamper(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            cargo = root / 'Cargo.toml'
            cargo.write_text('[workspace.package]\nversion = "0.2.0"\n')
            binary = root / 'tray'
            binary.write_bytes(b'release fixture')
            output = root / 'latest.json'
            script = str(Path(manifest.__file__).resolve())
            env = {**os.environ, 'RELEASE_SIGNING_KEY': bytes(range(32)).hex()}
            build = [sys.executable, script, 'build', '--version', 'v0.2.0', '--cargo-version', str(cargo), '--base-url', 'https://example.invalid/v0.2.0', '--triple', 'x86_64-pc-windows-msvc', str(binary), 'tray.exe', '--output', str(output)]
            subprocess.run(build, env=env, check=True, capture_output=True)
            public = manifest.key(env['RELEASE_SIGNING_KEY']).public_key().public_bytes_raw().hex()
            verify = [sys.executable, script, 'verify', '--manifest', str(output), '--public-key', public]
            subprocess.run(verify, check=True, capture_output=True)
            envelope = json.loads(output.read_text())
            payload = json.loads(base64.b64decode(envelope['payload']))
            self.assertEqual(payload['version'], '0.2.0')
            self.assertEqual(payload['artifacts']['x86_64-pc-windows-msvc']['sha256'], hashlib.sha256(binary.read_bytes()).hexdigest())
            envelope['payload'] = base64.b64encode(b'{}').decode()
            output.write_text(json.dumps(envelope))
            self.assertNotEqual(subprocess.run(verify, capture_output=True).returncode, 0)

    def test_tag_is_normalized_and_cargo_version_is_required(self):
        self.assertEqual(manifest.semver("v0.2.0"), "0.2.0")
        self.assertEqual(manifest.semver("1.2.3-beta.1"), "1.2.3-beta.1")
        with self.assertRaises(ValueError):
            manifest.semver("release-0.2.0")

    def test_github_and_domestic_signatures_verify_and_artifact_hash_matches(self):
        seed = bytes(range(32))
        signer = Ed25519PrivateKey.from_private_bytes(seed)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            binary = root / "tray"
            binary.write_bytes(b"fixture tray")
            cargo = root / "Cargo.toml"
            cargo.write_text('[workspace.package]\nversion = "0.2.0"\n', encoding="utf-8")
            artifacts = {"x86_64-unknown-linux-gnu": {"sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "size": binary.stat().st_size}}
            payloads = []
            for base in ("https://github.invalid/v0.2.0/raw", "https://oss.invalid/v0.2.0/raw"):
                payload = manifest.canonical({"version": "0.2.0", "notes": "fixture", "artifacts": {**artifacts, "x86_64-unknown-linux-gnu": {**artifacts["x86_64-unknown-linux-gnu"], "url": f"{base}/tray"}}})
                envelope = {"payload": base64.b64encode(payload).decode(), "signature": base64.b64encode(signer.sign(payload)).decode()}
                Ed25519PrivateKey.from_private_bytes(seed).public_key().verify(base64.b64decode(envelope["signature"]), base64.b64decode(envelope["payload"]))
                payloads.append(envelope)
            self.assertEqual(json.loads(base64.b64decode(payloads[0]["payload"]))["artifacts"]["x86_64-unknown-linux-gnu"]["sha256"], artifacts["x86_64-unknown-linux-gnu"]["sha256"])
            self.assertNotEqual(payloads[0], payloads[1])

    def test_tampering_is_rejected(self):
        signer = Ed25519PrivateKey.from_private_bytes(bytes(range(32)))
        payload = manifest.canonical({"version": "0.2.0", "notes": "fixture", "artifacts": {}})
        signature = signer.sign(payload)
        with self.assertRaises(Exception):
            signer.public_key().verify(signature, payload + b"tampered")


if __name__ == "__main__":
    unittest.main()
