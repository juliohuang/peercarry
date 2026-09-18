"""Refuse promoting an older or prerelease version to the stable channel."""
import argparse
import json
import re


def stable_version(value):
    if not re.fullmatch(r"v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", value):
        raise ValueError("stable releases must use vMAJOR.MINOR.PATCH")
    return tuple(map(int, value.removeprefix("v").split(".")))


def validate(version, releases):
    current = stable_version(version)
    for release in releases:
        if release.get("isDraft") or release.get("isPrerelease"):
            continue
        try:
            previous = stable_version(release["tagName"])
        except ValueError:
            continue
        if previous > current:
            raise ValueError("a newer stable release already exists; refusing channel downgrade")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--published", required=True)
    args = parser.parse_args()
    with open(args.published, encoding="utf-8") as source:
        validate(args.version, json.load(source))
