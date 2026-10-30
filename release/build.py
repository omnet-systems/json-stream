#!/usr/bin/env python3
"""Offline release build with build paths remapped (stdlib only).

  python3 release/build.py [--cargo-home .work/cargo-home]

Builds `cargo build --release --locked --offline` with a local CARGO_HOME,
keeps any RUSTFLAGS already in the environment and appends --remap-path-prefix for the user's home, the project
root and the cargo home. Flags go through CARGO_ENCODED_RUSTFLAGS in the child env only. The resulting ELF is
scanned for the home path (as given and resolved) and the build fails (printing only a count) if it is present.
Writes .work/build-info.json (private, no path values)."""
import argparse, json, os, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "release" / "json-stream"

def homes():
    """Home as given and as resolved (rustc may embed either when a component is a symlink)."""
    return list(dict.fromkeys([Path.home(), Path.home().resolve()]))

def home_hits(path):
    data = Path(path).read_bytes()
    return sum(data.count(str(h).encode()) for h in homes())

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cargo-home", type=Path, default=ROOT / ".work" / "cargo-home")
    a = ap.parse_args()
    ch = a.cargo_home.resolve()
    env = dict(os.environ, CARGO_HOME=str(ch))
    # cargo splits RUSTFLAGS on whitespace only (no shell quoting), so plain split() is the right semantics
    if "CARGO_ENCODED_RUSTFLAGS" in env:
        flags = [f for f in env["CARGO_ENCODED_RUSTFLAGS"].split("\x1f") if f]
    else:
        flags = env.get("RUSTFLAGS", "").split()
    # rustc: when several prefixes match, the last one wins, so list general -> specific
    base = len(flags)
    for src, dst in [(h, "/build/user") for h in homes()] + [(ROOT, "/build/json-stream"), (ch, "/build/cargo-home")]:
        flags.append(f"--remap-path-prefix={src}={dst}")
    env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
    env.pop("RUSTFLAGS", None)
    cmd = ["cargo", "build", "--release", "--locked", "--offline"]
    if subprocess.run(cmd, cwd=ROOT, env=env).returncode:
        sys.exit("build.py: cargo build failed")
    n = home_hits(BIN)
    if n:
        sys.exit(f"build.py: PRIVACY FAIL: binary contains the build-home path {n} time(s) (value withheld)")
    info = {"remap_applied": True, "remap_rules": len(flags) - base, "inherited_rustflags": base,
            "home_marker_hits": 0, "offline": "--offline" in cmd}
    (ROOT / ".work").mkdir(exist_ok=True)
    (ROOT / ".work" / "build-info.json").write_text(json.dumps(info, indent=1) + "\n")
    print("build ok; home-path marker hits: 0")

if __name__ == "__main__":
    main()
