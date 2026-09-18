#!/usr/bin/env python3
"""Build and verify the signed latest.json update envelope."""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
from pathlib import Path
from urllib.parse import urlsplit

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey


def canonical(value: object) -> bytes:
    return (json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True) + "\n").encode("utf-8")


def key(seed: str) -> Ed25519PrivateKey:
    raw = bytes.fromhex(seed)
    if len(raw) != 32:
        raise ValueError("RELEASE_SIGNING_KEY must be exactly 32-byte seed hex")
    return Ed25519PrivateKey.from_private_bytes(raw)


def semver(tag: str) -> str:
    match = re.fullmatch(r"v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", tag)
    if not match:
        raise ValueError(f"invalid release version: {tag!r}")
    return tag[1:] if tag.startswith("v") else tag


def check_cargo_version(path: Path, version: str) -> None:
    text = path.read_text(encoding="utf-8")
    match = re.search(r"(?ms)^\[workspace\.package\].*?^version\s*=\s*\"([^\"]+)\"", text)
    if not match or match.group(1) != version:
        raise ValueError(f"release version {version} does not match workspace Cargo.toml")


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    build = sub.add_parser("build")
    build.add_argument("--version", required=True)
    build.add_argument("--cargo-version", type=Path, required=True)
    build.add_argument("--notes", default="")
    build.add_argument("--base-url", required=True)
    build.add_argument("--triple", action="append", nargs=3, metavar=("TRIPLE", "TRAY", "RAW_NAME"), required=True)
    build.add_argument("--output", type=Path, required=True)
    build.add_argument("--key", default=os.environ.get("RELEASE_SIGNING_KEY", ""))
    verify = sub.add_parser("verify")
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--public-key", required=True)
    args = parser.parse_args()
    if args.command == "build":
        url = urlsplit(args.base_url)
        if url.scheme != "https" or not url.hostname or url.username or url.password or url.query or url.fragment:
            raise ValueError("release base URL must be HTTPS without credentials, query or fragment")
        version = semver(args.version)
        check_cargo_version(args.cargo_version, version)
        signer = key(args.key)
        artifacts = {}
        for triple, tray, raw_name in args.triple:
            if triple in artifacts or not re.fullmatch(r"[A-Za-z0-9._-]+", raw_name):
                raise ValueError("duplicate target or invalid artifact name")
            path = Path(tray)
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            artifacts[triple] = {"url": f"{args.base_url.rstrip('/')}/{raw_name}", "sha256": digest, "size": path.stat().st_size}
        payload = canonical({"version": version, "notes": args.notes, "artifacts": artifacts})
        envelope = {"payload": base64.b64encode(payload).decode("ascii"), "signature": base64.b64encode(signer.sign(payload)).decode("ascii")}
        args.output.write_text(json.dumps(envelope, ensure_ascii=False, separators=(",", ":")) + "\n", encoding="utf-8")
    else:
        envelope = json.loads(args.manifest.read_text(encoding="utf-8"))
        payload = base64.b64decode(envelope["payload"], validate=True)
        signature = base64.b64decode(envelope["signature"], validate=True)
        public = bytes.fromhex(args.public_key)
        if len(public) != 32:
            raise ValueError("public key must be 32-byte hex")
        Ed25519PublicKey.from_public_bytes(public).verify(signature, payload)
        json.loads(payload.decode("utf-8"))
        print("manifest signature: OK")


if __name__ == "__main__":
    main()
