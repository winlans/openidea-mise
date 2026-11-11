#!/usr/bin/env python3
"""Fail when release versions or the expected package name disagree."""

from __future__ import annotations

import argparse
import pathlib
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
CRATE_NAME = "openidea-mise"
PLUGIN_ID = "winlans.mise"


def read_toml(path: pathlib.Path) -> dict:
    with path.open("rb") as source:
        return tomllib.load(source)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--expected", help="require this version")
    parser.add_argument("--print-version", action="store_true")
    args = parser.parse_args()

    cargo = read_toml(ROOT / "Cargo.toml")
    plugin = read_toml(ROOT / "plugin.toml")
    lock = read_toml(ROOT / "Cargo.lock")
    cargo_version = cargo["package"]["version"]
    plugin_version = plugin["version"]
    lock_versions = {
        package["version"]
        for package in lock["package"]
        if package["name"] == CRATE_NAME
    }

    errors: list[str] = []
    if plugin["id"] != PLUGIN_ID:
        errors.append(f"plugin.toml id is {plugin['id']!r}, expected {PLUGIN_ID!r}")
    if cargo_version != plugin_version:
        errors.append(
            f"Cargo.toml version {cargo_version} != plugin.toml version {plugin_version}"
        )
    if lock_versions != {cargo_version}:
        errors.append(
            f"Cargo.lock versions for {CRATE_NAME} are {sorted(lock_versions)}, "
            f"expected only {cargo_version}"
        )
    if args.expected and cargo_version != args.expected:
        errors.append(
            f"release version {cargo_version} != expected version {args.expected}"
        )

    package_name = f"{PLUGIN_ID}-{cargo_version}.oiplugin"
    if errors:
        for error in errors:
            print(f"version check: {error}", file=sys.stderr)
        return 1
    if args.print_version:
        print(cargo_version)
    else:
        print(f"versions agree at {cargo_version}; package is {package_name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
