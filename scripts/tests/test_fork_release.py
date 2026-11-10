"""Contract tests for the manual fork release packaging and metadata."""
import io
import json
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

from scripts.fork_release import release_manifest, stamp


class ForkReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def package_json(self, target, build):
        return json.dumps(
            {
                "version": "0.167.0",
                "target": target,
                "entrypoint": "bin/codex.exe" if target.endswith("msvc") else "bin/codex",
                "forkBuildNumber": build,
            }
        ).encode()

    def archives(self, build=3):
        windows = self.root / "codex-windows-x64.zip"
        macos = self.root / "codex-macos-intel.tar.gz"
        with zipfile.ZipFile(windows, "w") as z:
            z.writestr("codex-package.json", self.package_json("x86_64-pc-windows-msvc", build))
            z.writestr("bin/codex.exe", b"fake executable")
        with tarfile.open(macos, "w:gz") as t:
            for name, data in [
                ("codex-package.json", self.package_json("x86_64-apple-darwin", build)),
                ("bin/codex", b"fake executable"),
            ]:
                entry = tarfile.TarInfo(name)
                entry.size = len(data)
                entry.mode = 0o755
                t.addfile(entry, io.BytesIO(data))
        return windows, macos

    def test_stamp_updates_build_number(self):
        package = self.root / "package"
        package.mkdir()
        manifest = package / "codex-package.json"
        manifest.write_text(
            '{"version":"0.167.0","target":"x86_64-apple-darwin"}',
            encoding="utf-8",
        )
        stamp(package, 4)
        self.assertEqual(json.loads(manifest.read_text())["forkBuildNumber"], 4)

    def test_publish_requires_matching_builds(self):
        win, mac = self.archives(3)
        manifest = release_manifest("fork-v0.167.0.3", win, mac)
        self.assertEqual(manifest["version"], "0.167.0")
        self.assertEqual(manifest["build"], 3)
        self.assertEqual(len(manifest["assets"]["x86_64-pc-windows-msvc"]["sha256"]), 64)
        self.assertEqual(len(manifest["assets"]["x86_64-apple-darwin"]["sha256"]), 64)
        with self.assertRaises(ValueError):
            release_manifest("fork-v0.167.0.4", win, mac)
        with self.assertRaises(ValueError):
            release_manifest("rust-v0.167.0", win, mac)

    def test_missing_or_wrong_platform_rejected(self):
        win, mac = self.archives(3)
        with self.assertRaises(ValueError):
            release_manifest("fork-v0.168.0.3", win, mac)
        mac.unlink()
        with self.assertRaises(FileNotFoundError):
            release_manifest("fork-v0.167.0.3", win, mac)


if __name__ == "__main__":
    unittest.main()
