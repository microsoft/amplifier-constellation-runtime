#!/usr/bin/env python3
"""Package one exact native executable; no build tools are needed to run it."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import stat
import subprocess
import tarfile
import tomllib
import zipfile

from native_binary import inspect

PLATFORMS = {f"{os}-{arch}" for os in ("linux", "macos", "windows")
             for arch in ("x86_64", "aarch64")}
ROOT = Path(__file__).resolve().parents[1]


def package(binary, platform, source, output):
    if platform not in PLATFORMS or not re.fullmatch(r"[0-9a-f]{40}", source):
        raise ValueError("Provide an exact native platform and source commit")
    if not binary.is_file() or binary.is_symlink():
        raise ValueError("Provide the compiled ordinary executable")
    data = binary.read_bytes()
    metadata = inspect(data, platform)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    actual = subprocess.run([str(binary.resolve()), "--version"], check=True,
                            capture_output=True, text=True, timeout=15).stdout.strip()
    if actual != "agent-runtime " + version:
        raise ValueError("Executable version does not match source")
    manifest = {"schema": 1, "version": version, "platform": platform,
                "source_commit": source, "binary": metadata,
                "signed_or_notarized": False}
    name = "agent-runtime.exe" if platform.startswith("windows-") else "agent-runtime"
    entries = [(name, data, 0o755), ("LICENSE", (ROOT / "LICENSE").read_bytes(), 0o644),
               ("PACKAGE.json", (json.dumps(manifest, indent=2) + "\n").encode(), 0o644)]
    output.mkdir(parents=True, exist_ok=True)
    suffix = ".zip" if platform.startswith("windows-") else ".tar.gz"
    archive = output / f"agent-runtime-{version}-{platform}{suffix}"
    if suffix == ".zip":
        with zipfile.ZipFile(archive, "w") as zipped:
            for path, content, mode in entries:
                info = zipfile.ZipInfo(path)
                info.create_system = 3
                info.external_attr = (stat.S_IFREG | mode) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                zipped.writestr(info, content)
    else:
        with archive.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as tar:
                for path, content, mode in entries:
                    info = tarfile.TarInfo(path)
                    info.size, info.mode, info.mtime = len(content), mode, 0
                    tar.addfile(info, io.BytesIO(content))
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(checksum + "  " + archive.name + "\n")
    archive.with_name(archive.name + ".json").write_text(json.dumps(
        {**manifest, "filename": archive.name, "sha256": checksum}, indent=2) + "\n")
    return archive


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--platform", choices=sorted(PLATFORMS), required=True)
    parser.add_argument("--source", required=True)
    parser.add_argument("--output", type=Path, default=Path("dist"))
    args = parser.parse_args()
    print(package(args.binary, args.platform, args.source, args.output))
