#!/usr/bin/env python3
"""Idempotently add a signed release to the OpenIDEA community registry."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import sys
import tomllib
import zipfile

PLUGIN_ID = "winlans.mise"
PUBLISHER = "winlans"
REPOSITORY = "https://github.com/winlans/openidea-mise"
SEMVER = re.compile(
    r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)"
    r"(?:[-+][0-9A-Za-z.-]+)?\Z"
)
SHA256 = re.compile(r"[0-9a-f]{64}\Z")


def toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def load_toml(path: pathlib.Path) -> dict:
    with path.open("rb") as source:
        return tomllib.load(source)


def package_public_key(package: pathlib.Path) -> str:
    with zipfile.ZipFile(package) as archive:
        try:
            signature = json.loads(archive.read("SIGNATURE.json"))
        except KeyError as error:
            raise ValueError("package is not signed: SIGNATURE.json is missing") from error
    if signature.get("publisher") != PUBLISHER:
        raise ValueError(
            f"package is signed by {signature.get('publisher')!r}, expected {PUBLISHER!r}"
        )
    key = signature.get("key")
    if not isinstance(key, str) or not key:
        raise ValueError("SIGNATURE.json has no public key")
    return key


def ensure_publisher(path: pathlib.Path, public_key: str) -> bool:
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            "\n".join(
                [
                    'name = "winlans"',
                    'owners = ["winlans"]',
                    f"keys = [{toml_string(public_key)}]",
                    "revoked-keys = []",
                    "",
                ]
            ),
            encoding="utf-8",
        )
        return True

    publisher = load_toml(path)
    if "winlans" not in publisher.get("owners", []):
        raise ValueError(f"{path}: winlans is not listed as an owner")
    keys = publisher.get("keys", [])
    if public_key not in keys:
        raise ValueError(
            f"{path}: release uses an unregistered signing key; "
            "perform an explicit endorsed key rotation"
        )
    return False


def version_block(version: str, package_url: str, sha256: str) -> str:
    return "\n".join(
        [
            "[[versions]]",
            f"version = {toml_string(version)}",
            f"package = {toml_string(package_url)}",
            f"sha256 = {toml_string(sha256)}",
            "",
        ]
    )


def ensure_plugin(
    path: pathlib.Path, version: str, package_url: str, sha256: str
) -> bool:
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            f"repository = {toml_string(REPOSITORY)}\n\n"
            + version_block(version, package_url, sha256),
            encoding="utf-8",
        )
        return True

    plugin = load_toml(path)
    if plugin.get("repository") != REPOSITORY:
        raise ValueError(
            f"{path}: repository is {plugin.get('repository')!r}, expected {REPOSITORY!r}"
        )
    for item in plugin.get("versions", []):
        if item.get("version") != version:
            continue
        expected = {"version": version, "package": package_url, "sha256": sha256}
        actual = {key: item.get(key) for key in expected}
        if actual != expected:
            raise ValueError(
                f"{path}: version {version} already exists with different release data"
            )
        return False

    with path.open("a", encoding="utf-8") as output:
        if path.stat().st_size and not path.read_text(encoding="utf-8").endswith("\n"):
            output.write("\n")
        output.write("\n" + version_block(version, package_url, sha256))
    return True


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--registry", type=pathlib.Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--package", type=pathlib.Path, required=True)
    parser.add_argument("--sha256")
    args = parser.parse_args()

    if not SEMVER.fullmatch(args.version):
        parser.error(f"invalid semantic version: {args.version}")
    expected_name = f"{PLUGIN_ID}-{args.version}.oiplugin"
    if args.package.name != expected_name:
        parser.error(f"package must be named {expected_name}")
    digest = hashlib.sha256(args.package.read_bytes()).hexdigest()
    if args.sha256 and args.sha256 != digest:
        parser.error(f"package SHA-256 is {digest}, not {args.sha256}")
    if not SHA256.fullmatch(digest):
        parser.error("invalid SHA-256")

    public_key = package_public_key(args.package)
    package_url = (
        f"{REPOSITORY}/releases/download/v{args.version}/{expected_name}"
    )
    changed = ensure_publisher(
        args.registry / "publishers" / f"{PUBLISHER}.toml", public_key
    )
    changed |= ensure_plugin(
        args.registry / "plugins" / f"{PLUGIN_ID}.toml",
        args.version,
        package_url,
        digest,
    )
    print(f"changed={'true' if changed else 'false'}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, zipfile.BadZipFile) as error:
        print(f"registry update: {error}", file=sys.stderr)
        raise SystemExit(1)
