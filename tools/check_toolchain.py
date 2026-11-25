#!/usr/bin/env python3
"""Enforce the LLVM pin in toolchain.lock on the clang a build uses.

    check_toolchain.py <clang> [--require-stamp] [--allow-unpinned]

Strict (default): `clang --version` must report the pinned version AND the
pinned llvm-project commit (a clang built from the pinned checkout reports
`(<repository> <commit>)`). A different commit always fails.

--require-stamp: additionally, the install prefix (`<clang dir>/..`) must
hold the stamp tools/provision_llvm.py wrote after building the pinned
commit, recording that commit and the SHA-256 of this very clang binary.
CI uses this mode.

--allow-unpinned: for local development with a distribution clang, which
reports no commit. The version must still match, a reported commit must
still match, and the result says plainly that the commit is NOT enforced.
"""
import argparse, hashlib, json, os, re, shutil, subprocess, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from toolchain_lock import llvm_pin  # noqa: E402

STAMP = "pegasus-toolchain.json"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("clang")
    ap.add_argument("--require-stamp", action="store_true")
    ap.add_argument("--allow-unpinned", action="store_true")
    ap.add_argument("--lock")
    a = ap.parse_args()
    pin = llvm_pin(a.lock)
    want_v, want_c = pin["version"], pin["commit"]
    exe = shutil.which(a.clang) or a.clang
    try:
        out = subprocess.run([exe, "--version"], capture_output=True, text=True, timeout=60).stdout
    except OSError as e:
        sys.exit(f"FAIL: cannot run {a.clang}: {e}")
    first = out.splitlines()[0] if out else ""
    m = re.search(r"clang version (\d+\.\d+\.\d+)", first)
    got_v = m.group(1) if m else None
    c = re.search(r"\(\s*\S*llvm-project(?:\.git)?\s+([0-9a-f]{40})\s*\)", first)
    got_c = c.group(1) if c else None
    if got_v != want_v:
        sys.exit(f"FAIL: {a.clang} reports version {got_v or 'unknown'}; toolchain.lock pins {want_v}")
    if got_c is not None and got_c != want_c:
        sys.exit(f"FAIL: {a.clang} was built from llvm-project {got_c}; toolchain.lock pins {want_c}")
    if a.require_stamp:
        stamp = os.path.join(os.path.dirname(os.path.dirname(os.path.realpath(exe))), STAMP)
        if not os.path.exists(stamp):
            sys.exit(f"FAIL: no provisioning stamp {stamp} (run tools/provision_llvm.py)")
        s = json.load(open(stamp, encoding="utf-8"))
        if s.get("commit") != want_c or s.get("version") != want_v:
            sys.exit(f"FAIL: stamp records {s.get('commit')} / {s.get('version')}; toolchain.lock pins {want_c} / {want_v}")
        if s.get("clang_sha256") != sha256(os.path.realpath(exe)):
            sys.exit("FAIL: the clang binary does not match the SHA-256 recorded when it was provisioned")
    if got_c is None:
        if not a.allow_unpinned:
            sys.exit(f"FAIL: {a.clang} reports version {got_v} but no llvm-project commit; the pin "
                     f"{want_c[:12]} cannot be verified (provision with tools/provision_llvm.py, or pass "
                     f"--allow-unpinned / -DPEGASUS_ALLOW_UNPINNED_LLVM=ON for local development)")
        print(f"WARNING: clang {got_v} matches the pinned version, but reports no commit: the commit pin "
              f"{want_c[:12]} is NOT enforced (allowed by --allow-unpinned)")
        return
    print(f"clang {got_v} built from llvm-project {got_c[:12]} matches toolchain.lock ({pin['release']})"
          + (", provisioning stamp and binary hash verified" if a.require_stamp else ""))


main()
