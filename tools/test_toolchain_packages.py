#!/usr/bin/env python3
"""Tests for the toolchain package path (tools/package_llvm.py pack/record,
tools/fetch_llvm_package.py) on a fake install prefix, offline: multi-part
archives, the lock's hashes, the source-pin cross-check, safe unpacking and
the final pin check. POSIX only (fake executables, symlinks)."""
import hashlib, io, json, os, subprocess, sys, tarfile, tempfile, lzma

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from toolchain_lock import llvm_pin  # noqa: E402

if os.name == "nt":
    print("skipped on Windows (fake executables and symlinks)")
    sys.exit(0)

PIN = llvm_pin()
failures = 0


def run(args, env, ok, needle, name):
    global failures
    r = subprocess.run([sys.executable] + args, capture_output=True, text=True, env=env)
    text = r.stdout + r.stderr
    good = (r.returncode == 0) == ok and needle in text
    print(("ok   " if good else "FAIL ") + name)
    if not good:
        failures += 1
        print("     exit", r.returncode, text.strip()[-800:])
    return r


with tempfile.TemporaryDirectory() as t:
    prefix = os.path.join(t, "prefix")
    os.makedirs(os.path.join(prefix, "bin"))
    for d in ("lib/cmake/llvm", "lib/cmake/mlir", "include/mlir/IR"):
        os.makedirs(os.path.join(prefix, d))
    clang = os.path.join(prefix, "bin", "clang")
    open(clang, "w").write(f"#!/bin/sh\necho 'clang version {PIN['version']} "
                           f"(https://github.com/llvm/llvm-project.git {PIN['commit']})'\n")
    os.chmod(clang, 0o755)
    os.symlink("clang", os.path.join(prefix, "bin", "clang++"))
    os.symlink("cmake", os.path.join(prefix, "lib", "cmake-link"))  # a directory symlink
    open(os.path.join(prefix, "lib/cmake/mlir/MLIRConfig.cmake"), "wb").write(os.urandom(300_000))
    digest = hashlib.sha256(open(clang, "rb").read()).hexdigest()
    json.dump({"commit": PIN["commit"], "version": PIN["version"], "clang_sha256": digest},
              open(os.path.join(prefix, "pegasus-toolchain.json"), "w"))
    out = os.path.join(t, "dist")
    lock = os.path.join(t, "packages.lock.json")
    env = dict(os.environ, PEGASUS_PACKAGES_LOCK=lock, PEGASUS_PACKAGE_PART_SIZE="100000",
               GITHUB_REPOSITORY="example/pegasus", GITHUB_REF="refs/heads/main")
    run([os.path.join(HERE, "package_llvm.py"), "pack", "--prefix", prefix, "--platform", "linux-x86_64",
         "--out", out], env, True, "part(s)", "pack splits the archive into parts")
    mf = [os.path.join(out, f) for f in os.listdir(out) if f.endswith(".manifest.json")][0]
    m = json.load(open(mf))
    print(("ok   " if len(m["parts"]) >= 2 else "FAIL ") + f"multi-part archive ({len(m['parts'])} parts)")
    failures += len(m["parts"]) < 2
    fetch = [os.path.join(HERE, "fetch_llvm_package.py"), "--platform", "linux-x86_64", "--from-dir", out,
             "--no-attestation"]
    run(fetch + ["--dest", os.path.join(t, "d0")], env, False, "pins no package", "unpinned platform is refused")
    run([os.path.join(HERE, "package_llvm.py"), "record", "--manifest", mf], env, True, "recorded",
        "record writes the lock")
    r = run(fetch + ["--dest", os.path.join(t, "d1")], env, True, "matches toolchain.lock",
            "fetch verifies hashes, unpacks and passes the pin check")
    print(("ok   " if "NOT verified" in r.stdout else "FAIL ") + "--no-attestation says provenance is NOT verified")
    failures += "NOT verified" not in r.stdout
    same = (os.path.islink(os.path.join(t, "d1/bin/clang++")) and os.path.islink(os.path.join(t, "d1/lib/cmake-link"))
            and open(os.path.join(t, "d1/lib/cmake/mlir/MLIRConfig.cmake"), "rb").read()
            == open(os.path.join(prefix, "lib/cmake/mlir/MLIRConfig.cmake"), "rb").read())
    print(("ok   " if same else "FAIL ") + "unpacked tree equals the prefix (files, file and directory links)")
    failures += not same
    run(fetch + ["--dest", os.path.join(t, "d1")], env, False, "is not empty", "non-empty destination is refused")
    # A corrupted part: the part hash catches it.
    p0 = os.path.join(out, m["parts"][0]["name"])
    good = open(p0, "rb").read()
    open(p0, "wb").write(bytes([good[0] ^ 1]) + good[1:])
    run(fetch + ["--dest", os.path.join(t, "d2"), "--download-dir", os.path.join(t, "dl2")], env, False,
        "size or SHA-256 differs", "corrupted part is refused")
    open(p0, "wb").write(good)
    # The lock disagrees with the source pin.
    data = json.load(open(lock))
    data["packages"]["linux-x86_64"]["commit"] = "0" * 40
    json.dump(data, open(lock, "w"))
    run(fetch + ["--dest", os.path.join(t, "d3")], env, False, "toolchain.lock pins", "package for another commit is refused")
    # An archive with a member escaping the destination: hashes match (the
    # lock is regenerated for it), unpacking must still refuse it.
    evil = os.path.join(t, "evil")
    os.makedirs(evil)
    buf = io.BytesIO()
    with lzma.open(buf, "wb") as xz, tarfile.open(fileobj=xz, mode="w") as tar:
        ti = tarfile.TarInfo("../escape.txt")
        ti.size = 4
        tar.addfile(ti, io.BytesIO(b"evil"))
    name = m["archive"]
    open(os.path.join(evil, name + ".000"), "wb").write(buf.getvalue())
    h = hashlib.sha256(buf.getvalue()).hexdigest()
    m2 = dict(m, sha256=h, size=len(buf.getvalue()),
              parts=[{"name": name + ".000", "sha256": h, "size": len(buf.getvalue())}])
    json.dump(m2, open(os.path.join(evil, "m.json"), "w"))
    os.remove(lock)
    run([os.path.join(HERE, "package_llvm.py"), "record", "--manifest", os.path.join(evil, "m.json")], env, True,
        "recorded", "record the hostile package")
    run([os.path.join(HERE, "fetch_llvm_package.py"), "--platform", "linux-x86_64", "--from-dir", evil,
         "--no-attestation", "--dest", os.path.join(t, "d4")], env, False, "unsafe archive member refused",
        "member escaping the destination is refused")
    print(("ok   " if not os.path.exists(os.path.join(t, "escape.txt")) else "FAIL ") + "nothing written outside")
    failures += os.path.exists(os.path.join(t, "escape.txt"))

print(f"{failures} failures")
sys.exit(1 if failures else 0)
