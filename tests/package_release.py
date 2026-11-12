"""Native archive identity and runtime-dependency boundaries; no executables run."""
import hashlib
import importlib.util
import json
from pathlib import Path
import stat
import struct
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from native_binary import inspect, DEFAULT_PE_BYTES, MAX_PE_BYTES

spec = importlib.util.spec_from_file_location("pack", ROOT / "scripts/package-release.py")
pack = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pack)


def pe(architecture="x86_64", imported="KERNEL32.dll", delayed=False):
    data = bytearray(0x800)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3c, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HH", data, 0x84, 0x8664 if architecture == "x86_64" else 0xaa64, 1)
    struct.pack_into("<H", data, 0x94, 240)
    optional = 0x98
    struct.pack_into("<H", data, optional, 0x20b)
    struct.pack_into("<I", data, optional + 60, 0x200)
    struct.pack_into("<I", data, optional + 108, 16)
    section = optional + 240
    struct.pack_into("<IIII", data, section + 8, 0x600, 0x1000, 0x600, 0x200)
    directory, size = (13, 64) if delayed else (1, 40)
    struct.pack_into("<II", data, optional + 112 + directory * 8, 0x1000, size)
    if delayed:
        struct.pack_into("<8I", data, 0x200, 1, 0x1100, 0, 0, 0, 0, 0, 0)
    else:
        struct.pack_into("<5I", data, 0x200, 0, 0, 0, 0x1100, 0)
    data[0x300:0x300 + len(imported) + 1] = imported.encode() + b"\0"
    return data


def elf(architecture="x86_64"):
    data = bytearray(64)
    data[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<HH", data, 16, 2, 62 if architecture == "x86_64" else 183)
    return data


def macho(architecture="x86_64"):
    data = bytearray(56)
    data[:4] = b"\xcf\xfa\xed\xfe"
    struct.pack_into("<IIIII", data, 4, 0x01000007 if architecture == "x86_64" else 0x0100000c, 0, 2, 1, 24)
    struct.pack_into("<IIIIII", data, 32, 0x32, 24, 1, 0xb0000, 0xf0000, 0)
    return data


class Packages(unittest.TestCase):
    def test_all_six_architectures_are_checked_from_executable_headers(self):
        for family, make in (("windows", pe), ("linux", elf), ("macos", macho)):
            for architecture in ("x86_64", "aarch64"):
                with self.subTest(family=family, architecture=architecture):
                    self.assertEqual(inspect(make(architecture), family + "-" + architecture)["architecture"], architecture)
                    wrong = "x86_64" if architecture == "aarch64" else "aarch64"
                    with self.assertRaises(ValueError):
                        inspect(make(architecture), family + "-" + wrong)

    def test_vc_runtime_ordinary_and_delayed_imports_refuse_packaging(self):
        for architecture in ("x86_64", "aarch64"):
            for name, delayed in (("VCRUNTIME140.dll", False), ("mSvCp140_2.DLL", True)):
                with self.subTest(architecture=architecture, name=name), self.assertRaises(ValueError):
                    inspect(pe(architecture, name, delayed), "windows-" + architecture)
        self.assertTrue(inspect(pe(imported="api-ms-win-crt-runtime-l1-1-0.dll"), "windows-x86_64")["standalone_vc_runtime"])

    def test_truncated_binary_metadata_is_never_packaged(self):
        for platform, data in (("windows-x86_64", pe()[:200]), ("macos-x86_64", macho()[:45]), ("linux-x86_64", elf()[:20])):
            with self.subTest(platform=platform), self.assertRaises(ValueError):
                inspect(data, platform)

    def test_large_dependency_needs_explicit_bounded_budget_without_import_bypass(self):
        data = pe()
        data.extend(bytes(DEFAULT_PE_BYTES + 1 - len(data)))
        with self.assertRaisesRegex(ValueError, "bounded Windows"):
            inspect(data, "windows-x86_64")
        self.assertTrue(inspect(data, "windows-x86_64", max_pe_bytes=MAX_PE_BYTES)["standalone_vc_runtime"])
        # The larger budget neither changes architecture nor permits VC imports.
        with self.assertRaisesRegex(ValueError, "architecture"):
            inspect(data, "windows-aarch64", max_pe_bytes=MAX_PE_BYTES)
        bad = pe(imported="VCRUNTIME140.dll", delayed=True)
        data[:len(bad)] = bad
        with self.assertRaisesRegex(ValueError, "external Visual C"):
            inspect(data, "windows-x86_64", max_pe_bytes=MAX_PE_BYTES)
        for invalid in (None, True, -1, 0, MAX_PE_BYTES + 1):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(ValueError, "budget"):
                inspect(data, "windows-x86_64", max_pe_bytes=invalid)

    def test_platform_archive_layout_checksum_and_repeatability(self):
        version = pack.tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
        for platform, data in (("linux-aarch64", elf("aarch64")), ("windows-aarch64", pe("aarch64"))):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                binary = root / "input"
                binary.write_bytes(data)
                with patch.object(pack.subprocess, "run") as run:
                    run.return_value.stdout = "agent-runtime " + version + "\n"
                    path = pack.package(binary, platform, "a" * 40, root / "dist")
                    before = path.read_bytes()
                    self.assertEqual(path, pack.package(binary, platform, "a" * 40, root / "dist"))
                    self.assertEqual(before, path.read_bytes())
                manifest = json.loads(path.with_name(path.name + ".json").read_text())
                self.assertEqual(manifest["binary"]["binary_sha256"], hashlib.sha256(data).hexdigest())
                self.assertEqual(manifest["sha256"], hashlib.sha256(before).hexdigest())
                self.assertEqual(manifest["source_commit"], "a" * 40)
                if platform.startswith("windows"):
                    with zipfile.ZipFile(path) as zipped:
                        self.assertEqual(set(zipped.namelist()), {"agent-runtime.exe", "LICENSE", "PACKAGE.json"})
                        self.assertEqual(stat.S_IMODE(zipped.getinfo("agent-runtime.exe").external_attr >> 16), 0o755)
                else:
                    with tarfile.open(path) as tar:
                        self.assertEqual(set(tar.getnames()), {"agent-runtime", "LICENSE", "PACKAGE.json"})
                        self.assertEqual(tar.getmember("agent-runtime").mode, 0o755)

    def test_wrong_version_or_provenance_leaves_no_archive(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            binary = root / "input"
            binary.write_bytes(elf())
            with patch.object(pack.subprocess, "run") as run:
                run.return_value.stdout = "other-program 0.0.0"
                with self.assertRaises(ValueError):
                    pack.package(binary, "linux-x86_64", "a" * 40, root / "dist")
            self.assertFalse((root / "dist").exists())
            with self.assertRaises(ValueError):
                pack.package(binary, "linux-x86_64", "moving-main", root / "dist")


if __name__ == "__main__":
    unittest.main(verbosity=2)
