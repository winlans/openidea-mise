from __future__ import annotations

import hashlib
import json
import pathlib
import tempfile
import unittest
import zipfile

import update_registry


class RegistryUpdateTests(unittest.TestCase):
    def package(self, root: pathlib.Path, version: str, key: str = "public-key") -> pathlib.Path:
        package = root / f"winlans.mise-{version}.oiplugin"
        with zipfile.ZipFile(package, "w") as archive:
            archive.writestr("plugin.toml", f'version = "{version}"')
            archive.writestr(
                "SIGNATURE.json",
                json.dumps(
                    {
                        "publisher": "winlans",
                        "key": key,
                        "signature": "signature",
                    }
                ),
            )
        return package

    def test_creates_entries_and_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            registry = root / "registry"
            package = self.package(root, "0.1.0")
            digest = hashlib.sha256(package.read_bytes()).hexdigest()
            publisher = registry / "publishers" / "winlans.toml"
            plugin = registry / "plugins" / "winlans.mise.toml"

            self.assertTrue(update_registry.ensure_publisher(publisher, "public-key"))
            self.assertTrue(
                update_registry.ensure_plugin(
                    plugin,
                    "0.1.0",
                    "https://example.test/package",
                    digest,
                )
            )
            self.assertFalse(update_registry.ensure_publisher(publisher, "public-key"))
            self.assertFalse(
                update_registry.ensure_plugin(
                    plugin,
                    "0.1.0",
                    "https://example.test/package",
                    digest,
                )
            )
            self.assertEqual(update_registry.package_public_key(package), "public-key")
            self.assertEqual(len(update_registry.load_toml(plugin)["versions"]), 1)

    def test_refuses_implicit_key_rotation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            publisher = pathlib.Path(directory) / "publishers" / "winlans.toml"
            update_registry.ensure_publisher(publisher, "first-key")
            with self.assertRaisesRegex(ValueError, "key rotation"):
                update_registry.ensure_publisher(publisher, "second-key")


if __name__ == "__main__":
    unittest.main()
