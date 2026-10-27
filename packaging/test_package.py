"""Packaging regressions that can run on every build host."""
import os
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package


def write_pe(path: Path, machine: int = 0x8664) -> None:
    data = bytearray(70)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 64)
    data[64:68] = b"PE\0\0"
    struct.pack_into("<H", data, 68, machine)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


class WindowsPackagingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.binaries = self.root / "built binaries"
        self.runtime = self.root / "redistributable files"
        for name in ("irori.exe", "irori-sign-in.exe"):
            write_pe(self.binaries / name)
        for name in package.WINDOWS_CRT_FILES:
            write_pe(self.runtime / name)
        environment = patch.dict(os.environ, {"IRORI_WINDOWS_CRT_DIR": str(self.runtime)})
        environment.start()
        self.addCleanup(environment.stop)

    def test_windows_archive_contains_runtime_next_to_both_executables(self):
        archive = package.package("windows", self.binaries, self.root / "output")
        with zipfile.ZipFile(archive) as zipped:
            for name in ("irori.exe", "irori-sign-in.exe", "vcruntime140.dll", "vcruntime140_1.dll", "install.ps1"):
                self.assertIn("Irori/" + name, zipped.namelist())
            self.assertEqual(zipped.read("Irori/vcruntime140.dll"), (self.runtime / "vcruntime140.dll").read_bytes())

    def test_missing_runtime_fails_before_creating_output(self):
        (self.runtime / "vcruntime140_1.dll").unlink()
        with self.assertRaisesRegex(FileNotFoundError, "redistributable DLLs"):
            package.package("windows", self.binaries, self.root / "output")
        self.assertFalse((self.root / "output").exists())

    def test_arm_runtime_cannot_be_bundled_with_x64_application(self):
        write_pe(self.runtime / "vcruntime140.dll", 0xAA64)
        with self.assertRaisesRegex(ValueError, "requires x64"):
            package.package("windows", self.binaries, self.root / "output")
        self.assertFalse((self.root / "output").exists())

    def test_x86_application_cannot_be_bundled_with_x64_runtime(self):
        write_pe(self.binaries / "irori.exe", 0x014C)
        with self.assertRaisesRegex(ValueError, "requires x64"):
            package.package("windows", self.binaries, self.root / "output")

    def test_invalid_runtime_binary_is_rejected(self):
        (self.runtime / "vcruntime140.dll").write_text("not a DLL")
        with self.assertRaisesRegex(ValueError, "Not a Windows executable"):
            package.package("windows", self.binaries, self.root / "output")

    def test_developer_prompt_redist_directory_is_discovered(self):
        root = self.root / "toolchain redist"
        expected = root / "x64/Microsoft.VC143.CRT"
        for name in package.WINDOWS_CRT_FILES:
            write_pe(expected / name)
        with patch.dict(os.environ, {"IRORI_WINDOWS_CRT_DIR": "", "VCToolsRedistDir": str(root), "ProgramFiles(x86)": str(self.root / "absent") }):
            self.assertEqual(package.windows_crt_directory(), expected)


if __name__ == "__main__":
    unittest.main()
