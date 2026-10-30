#!/usr/bin/env python3
"""Package the native Linux release using only the Python 3.11+ standard library."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import struct
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent


def fingerprint(data):
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def source_files():
    files = [ROOT / name for name in (".gitignore", "Cargo.toml", "Cargo.lock", "LICENSE", "README.md")]
    for folder in ("src", "tests", "fixtures", "fixtures-extra", "benchmarks", "release"):
        files.extend(p for p in (ROOT / folder).iterdir()
                     if p.is_file() and p.suffix in (".rs", ".py", ".json", ".txt"))
    return sorted(files)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/json-stream")
    args = parser.parse_args()
    binary = args.binary.resolve()
    binary.relative_to(ROOT)
    data = binary.read_bytes()
    # Derive the archive platform from the ELF, rather than a user-supplied label.
    if len(data) < 20 or data[:6] != b"\x7fELF\x02\x01":
        parser.error("expected a little-endian 64-bit ELF binary")
    machine = struct.unpack_from("<H", data, 18)[0]
    architectures = {62: "x86_64", 183: "aarch64"}
    if machine not in architectures:
        parser.error("unsupported ELF architecture")
    platform = "linux-" + architectures[machine]
    package = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
    version = package["version"]
    reported = subprocess.check_output([str(binary), "--version"], cwd=ROOT).decode().strip()
    if reported != "json-stream " + version:
        parser.error("binary version differs from Cargo.toml")
    files = {
        "json-stream": data,
        "LICENSE": (ROOT / "LICENSE").read_bytes(),
        "THIRD_PARTY_NOTICES.txt": (ROOT / "release/THIRD_PARTY_NOTICES.txt").read_bytes(),
        "USAGE.txt": subprocess.check_output([str(binary), "--help"], cwd=ROOT),
    }
    manifest = {
        "name": package["name"],
        "version": version,
        "platform": platform,
        "binary_sha256": fingerprint(data)["sha256"],
        "files": {name: fingerprint(contents) for name, contents in sorted(files.items())},
        "source_sha256": {p.relative_to(ROOT).as_posix(): fingerprint(p.read_bytes())["sha256"]
                          for p in source_files()},
    }
    files["MANIFEST.json"] = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
    name = "json-stream-" + version + "-" + platform
    (ROOT / "dist").mkdir(exist_ok=True)
    archive = ROOT / "dist" / (name + ".tar.gz")
    with archive.open("wb") as raw:
        with gzip.GzipFile(fileobj=raw, mode="wb", filename="", mtime=0, compresslevel=9) as zipped:
            with tarfile.open(fileobj=zipped, mode="w", format=tarfile.USTAR_FORMAT) as tar:
                directory = tarfile.TarInfo(name + "/")
                directory.type = tarfile.DIRTYPE
                directory.mode = 0o755
                directory.uid = directory.gid = directory.mtime = 0
                directory.uname = directory.gname = ""
                tar.addfile(directory)
                for filename, contents in sorted(files.items()):
                    member = tarfile.TarInfo(name + "/" + filename)
                    member.size = len(contents)
                    member.mode = 0o755 if filename == "json-stream" else 0o644
                    member.uid = member.gid = member.mtime = 0
                    member.uname = member.gname = ""
                    tar.addfile(member, io.BytesIO(contents))
    print(archive.relative_to(ROOT).as_posix())
    print("SHA256 " + fingerprint(archive.read_bytes())["sha256"])


if __name__ == "__main__":
    main()
