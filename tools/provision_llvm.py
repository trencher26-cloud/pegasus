#!/usr/bin/env python3
"""Provision the LLVM toolchain pinned in toolchain.lock: fetch the exact
llvm-project commit, verify it, build clang + MLIR, install, and stamp.

    provision_llvm.py --prefix DIR [--src DIR] [--build DIR] [--jobs N]
                      [--fetch-only] [--sparse PATH ...]

1. Fetch: `git fetch --depth 1 origin <commit>` into --src, check it out
   detached, and require `git rev-parse HEAD` to equal the pinned commit and
   the release tag to peel to it (`git ls-remote`). Git object ids are
   content hashes, so the checked-out tree is the pinned source.
2. Build (skipped by --fetch-only): CMake + Ninja, Release, host target,
   projects from the lock (clang;mlir), LLVM_APPEND_VC_REV=ON so the built
   clang reports the commit, tests/examples/benchmarks off, install to
   --prefix.
3. Verify and stamp: the installed clang must report the pinned version and
   commit; `<prefix>/pegasus-toolchain.json` records the commit, release,
   version and the SHA-256 of the installed clang binary. check_toolchain.py
   --require-stamp verifies all of it.

--sparse checks out only the given paths (for testing the fetch step).
On Windows, run from a Visual Studio developer environment.
"""
import argparse, hashlib, json, os, subprocess, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from toolchain_lock import llvm_pin  # noqa: E402

STAMP = "pegasus-toolchain.json"


def run(cmd, **kw):
    print("+ " + " ".join(cmd), flush=True)
    return subprocess.run(cmd, check=True, **kw)


def out(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw).stdout.strip()


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def fetch(pin, src, sparse):
    os.makedirs(src, exist_ok=True)
    git = ["git", "-C", src]
    if not os.path.isdir(os.path.join(src, ".git")):
        run(git + ["init", "-q"])
        run(git + ["remote", "add", "origin", pin["source"]])
    if sparse:
        run(git + ["sparse-checkout", "set", "--no-cone"] + [p if p.startswith("/") else "/" + p for p in sparse])
    run(git + ["fetch", "-q", "--depth", "1", "origin", pin["commit"]])
    run(git + ["checkout", "-q", "--detach", "FETCH_HEAD"])
    head = out(git + ["rev-parse", "HEAD"])
    if head != pin["commit"]:
        sys.exit(f"FAIL: checked out {head}, toolchain.lock pins {pin['commit']}")
    tag = out(git + ["ls-remote", "origin", f"refs/tags/{pin['release']}^{{}}"]).split()
    if not tag or tag[0] != pin["commit"]:
        sys.exit(f"FAIL: tag {pin['release']} peels to {tag[0] if tag else 'nothing'}, not {pin['commit']}")
    print(f"source verified: {pin['release']} = {head}")


def build(pin, src, bld, prefix, jobs):
    cfg = ["cmake", "-S", os.path.join(src, "llvm"), "-B", bld, "-G", "Ninja",
           "-DCMAKE_BUILD_TYPE=Release",
           f"-DCMAKE_INSTALL_PREFIX={prefix}",
           f"-DLLVM_ENABLE_PROJECTS={pin['projects']}",
           "-DLLVM_TARGETS_TO_BUILD=host",
           "-DLLVM_APPEND_VC_REV=ON",
           "-DLLVM_ENABLE_ASSERTIONS=OFF",
           "-DLLVM_INCLUDE_TESTS=OFF", "-DLLVM_INCLUDE_EXAMPLES=OFF", "-DLLVM_INCLUDE_BENCHMARKS=OFF",
           "-DLLVM_INCLUDE_DOCS=OFF", "-DMLIR_INCLUDE_TESTS=OFF", "-DCLANG_INCLUDE_TESTS=OFF",
           "-DLLVM_ENABLE_ZSTD=OFF", "-DLLVM_ENABLE_LIBXML2=OFF", "-DLLVM_ENABLE_BINDINGS=OFF"]
    run(cfg)
    b = ["cmake", "--build", bld, "--target", "install"]
    if jobs:
        b += ["--parallel", str(jobs)]
    run(b)


def stamp(pin, prefix):
    clang = os.path.join(prefix, "bin", "clang.exe" if os.name == "nt" else "clang")
    first = out([clang, "--version"]).splitlines()[0]
    if f"clang version {pin['version']}" not in first or pin["commit"] not in first:
        sys.exit(f"FAIL: built clang reports {first!r}; expected version {pin['version']} and commit {pin['commit']}")
    data = {"source": pin["source"], "release": pin["release"], "commit": pin["commit"],
            "version": pin["version"], "projects": pin["projects"], "clang_sha256": sha256(clang)}
    with open(os.path.join(prefix, STAMP), "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    print(f"stamped {os.path.join(prefix, STAMP)}: clang sha256 {data['clang_sha256']}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--prefix", required=True)
    ap.add_argument("--src")
    ap.add_argument("--build")
    ap.add_argument("--jobs", type=int)
    ap.add_argument("--fetch-only", action="store_true")
    ap.add_argument("--sparse", nargs="*")
    ap.add_argument("--lock")
    a = ap.parse_args()
    pin = llvm_pin(a.lock)
    prefix = os.path.abspath(a.prefix)
    src = os.path.abspath(a.src or prefix + "-src")
    fetch(pin, src, a.sparse)
    if a.fetch_only:
        return
    build(pin, src, os.path.abspath(a.build or prefix + "-build"), prefix, a.jobs)
    stamp(pin, prefix)


main()
