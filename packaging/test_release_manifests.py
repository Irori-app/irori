"""Package-manager manifests carry the release's version and checksums."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import release_manifests


class ReleaseManifestTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.dist = Path(temporary.name)
        self.sums = {}
        for name in ("Irori-1.2.3-linux.tar.gz", "Irori-1.2.3-macos-arm64.dmg",
                     "Irori-1.2.3-macos-x86_64.dmg", "Irori-1.2.3-windows.zip"):
            data = name.encode()
            (self.dist / name).write_bytes(data)
            self.sums[name] = hashlib.sha256(data).hexdigest()

    def test_every_manifest_names_the_release_and_its_checksums(self):
        out = release_manifests.render("1.2.3", self.dist, "irori-app/irori")
        pkgbuild = out["aur/PKGBUILD"]
        self.assertIn("pkgver=1.2.3\n", pkgbuild)
        self.assertIn(f"sha256sums=('{self.sums['Irori-1.2.3-linux.tar.gz']}')", pkgbuild)
        self.assertIn("releases/download/v$pkgver/Irori-$pkgver-linux.tar.gz", pkgbuild)

        cask = out["homebrew/Casks/irori.rb"]
        self.assertIn('version "1.2.3"', cask)
        self.assertIn(f'arm:   "{self.sums["Irori-1.2.3-macos-arm64.dmg"]}"', cask)
        self.assertIn(f'intel: "{self.sums["Irori-1.2.3-macos-x86_64.dmg"]}"', cask)
        self.assertIn("Irori-#{version}-macos-#{arch}.dmg", cask)
        self.assertIn('homepage "https://irori.app"', cask)
        self.assertIn('url="https://irori.app"', pkgbuild)

        scoop = json.loads(out["scoop/bucket/irori.json"])
        self.assertEqual(scoop["version"], "1.2.3")
        self.assertEqual(scoop["homepage"], "https://irori.app")
        self.assertEqual(scoop["architecture"]["64bit"]["hash"], self.sums["Irori-1.2.3-windows.zip"])
        self.assertTrue(scoop["architecture"]["64bit"]["url"].endswith("/v1.2.3/Irori-1.2.3-windows.zip"))

    def test_missing_archive_is_named(self):
        (self.dist / "Irori-1.2.3-macos-x86_64.dmg").unlink()
        with self.assertRaisesRegex(FileNotFoundError, "macos-x86_64.dmg"):
            release_manifests.render("1.2.3", self.dist, "irori-app/irori")


if __name__ == "__main__":
    unittest.main()
